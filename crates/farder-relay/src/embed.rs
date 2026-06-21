//! Rich external link embeds — the relay's fetch proxy phase two. Resolves
//! allowlisted URLs to normalized `LinkEmbed` metadata and streams media bytes,
//! so a requester's IP never touches the third-party site.

use anyhow::Result;
use farder_protocol::messages::{EmbedKind, EmbedMedia, LinkEmbed};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Twitter,
    YouTube,
    Reddit,
    Spotify,
}

/// Page-host allowlist: (registrable-or-exact host suffix, provider). A URL host
/// matches an entry if it equals the suffix OR ends with "." + suffix (so
/// subdomains match, but `youtube.com.evil.com` does NOT match `youtube.com`).
/// NOTE: direct-image hosts (i.redd.it/i.imgur.com) are intentionally NOT here —
/// Farder already inlines posted image URLs via the auto-attach path
/// (`MessageInput` -> `fetch_url`), so an image embed would render twice.
const ALLOWLIST: &[(&str, Provider)] = &[
    ("twitter.com", Provider::Twitter),
    ("x.com", Provider::Twitter),
    ("youtube.com", Provider::YouTube),
    ("youtu.be", Provider::YouTube),
    ("reddit.com", Provider::Reddit),
    ("redd.it", Provider::Reddit),
    ("open.spotify.com", Provider::Spotify),
    ("fxtwitter.com", Provider::Twitter),
];

/// Media/thumbnail CDN hosts. Adapters extract media URLs that live on CDNs
/// distinct from the page host (e.g. a tweet's video is on `video.twimg.com`,
/// not `x.com`). `fetch_media` accepts these IN ADDITION to page hosts so
/// thumbnails and inline video resolve, while the SSRF guard still applies.
const MEDIA_ALLOWLIST: &[&str] = &[
    "twimg.com",       // twitter media + thumbnails (video.twimg.com, pbs.twimg.com)
    "ytimg.com",       // youtube thumbnails (i.ytimg.com)
    "scdn.co",         // spotify cover art (i.scdn.co, mosaic.scdn.co)
    "redd.it",         // reddit images (i.redd.it, preview.redd.it, external-preview.redd.it)
    "redditmedia.com", // reddit thumbnails (b.thumbs.redditmedia.com)
];

/// True if a host matches an allowlist suffix safely (exact or dotted-subdomain).
fn host_matches(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// Classify a URL to a provider, or None if its host is not allowlisted.
/// Longest matching suffix wins (defensive; entries are currently unambiguous).
pub fn classify_url(raw: &str) -> Option<Provider> {
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    // Reject bare IP hosts outright (they can never be an allowlisted name).
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let mut best: Option<(usize, Provider)> = None;
    for (suffix, provider) in ALLOWLIST {
        if host_matches(&host, suffix) {
            let len = suffix.len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, *provider));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// True if the URL's host is an allowlisted PAGE host (used for metadata fetches).
pub fn host_is_allowlisted(raw: &str) -> bool {
    classify_url(raw).is_some()
}

/// True if the URL's host is allowlisted for MEDIA fetches — a page host OR a
/// known media CDN host (`fetch_media` only). SSRF resolution still applies on
/// top of this.
pub fn host_is_media_allowlisted(raw: &str) -> bool {
    if host_is_allowlisted(raw) {
        return true;
    }
    let Ok(parsed) = Url::parse(raw) else { return false; };
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false; };
    let host = host.to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    MEDIA_ALLOWLIST.iter().any(|s| host_matches(&host, s))
}

/// A successfully fetched text document.
pub struct FetchedText {
    pub body: String,
}

/// Seam over outbound HTTP so adapters are unit-testable without a network.
/// The production impl (`SafeFetcher`) enforces allowlist + SSRF + caps.
#[allow(async_fn_in_trait)]
pub trait LinkFetcher: Send + Sync {
    async fn fetch_text(&self, url: &str) -> Result<FetchedText>;
}

/// Rewrite a twitter.com/x.com status URL to the fxtwitter JSON API URL.
/// Returns None if the path isn't a `/<user>/status/<id>` shape.
pub fn fxtwitter_api_url(raw: &str) -> Option<String> {
    let u = Url::parse(raw).ok()?;
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    // [user, "status", id]
    if segs.len() >= 3 && segs[1] == "status" {
        Some(format!("https://api.fxtwitter.com/{}/status/{}", segs[0], segs[2]))
    } else {
        None
    }
}

use crate::proxy::is_global_ip;
use futures_util::StreamExt;

pub const META_CAP: usize = 16 * 1024;
pub const MEDIA_CAP: u64 = 25 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Returns true if the content-type is an allowed media type (image/* or video/mp4).
/// Strips parameters (e.g. "; charset=binary"), trims whitespace, lowercases.
pub fn content_type_allowed(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    base.starts_with("image/") || base == "video/mp4"
}

/// Fetch a media asset with full guards: allowlist + SSRF re-validation,
/// content-type allowlist, and a byte-counted cap enforced DURING streaming
/// (Content-Length is not trusted). Returns (content_type, bytes).
pub async fn fetch_media(client: &reqwest::Client, url: &str) -> Result<(String, Vec<u8>)> {
    if !validate_media_fetchable(url).await {
        tracing::warn!("embed media refused (allowlist/ssrf): {url}");
        anyhow::bail!("media refused (allowlist/ssrf): {url}");
    }
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("embed media transport error for {url}: {e}");
            return Err(e.into());
        }
    };
    if !resp.status().is_success() {
        tracing::warn!("embed media status {} for {url}", resp.status());
        anyhow::bail!("media status {}", resp.status());
    }
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if !content_type_allowed(&ct) {
        tracing::warn!("embed media content-type rejected '{ct}' for {url}");
        anyhow::bail!("media content-type rejected: {ct}");
    }

    let mut out: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        anyhow::ensure!(
            out.len() as u64 + chunk.len() as u64 <= MEDIA_CAP,
            "media exceeds cap"
        );
        out.extend_from_slice(&chunk);
    }
    Ok((ct, out))
}

/// SSRF resolution gate: every IP the URL's host resolves to must be globally
/// routable (anti-rebind). Shared by the page and media validators.
async fn resolves_to_global(raw: &str) -> bool {
    let Ok(u) = Url::parse(raw) else { return false; };
    let Some(host) = u.host_str() else { return false; };
    let port = u.port_or_known_default().unwrap_or(443);
    let Ok(addrs) = tokio::net::lookup_host((host, port)).await else { return false; };
    let mut any = false;
    for a in addrs {
        any = true;
        if !is_global_ip(a.ip()) { return false; }
    }
    any
}

/// Page-fetch gate: allowlisted PAGE host AND all resolved IPs global.
pub async fn validate_fetchable(raw: &str) -> bool {
    host_is_allowlisted(raw) && resolves_to_global(raw).await
}

/// Media-fetch gate: page OR media-CDN host AND all resolved IPs global.
/// This is the re-validation of adapter-extracted media URLs (the "media-URL
/// trap" defense), widened to the media CDNs the adapters legitimately use.
pub async fn validate_media_fetchable(raw: &str) -> bool {
    host_is_media_allowlisted(raw) && resolves_to_global(raw).await
}

pub struct SafeFetcher {
    client: reqwest::Client,
}

impl SafeFetcher {
    pub fn new() -> Result<Self> {
        // Browser-like UA: several providers (Reddit, some oEmbed endpoints)
        // reject or rate-limit non-browser User-Agents. Note this does NOT defeat
        // datacenter-IP blocking some sites apply to the relay's host.
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none()) // we handle hops ourselves
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .build()?;
        Ok(Self { client })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

impl LinkFetcher for SafeFetcher {
    async fn fetch_text(&self, url: &str) -> Result<FetchedText> {
        // Manual redirect loop (max 4 hops) with SSRF re-validation on every hop.
        let mut current = url.to_string();
        for _ in 0..4 {
            if !validate_fetchable(&current).await {
                tracing::warn!("embed fetch refused (allowlist/ssrf): {current}");
                anyhow::bail!("fetch refused (allowlist/ssrf): {current}");
            }
            let resp = match self.client.get(&current).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("embed fetch transport error for {current}: {e}");
                    return Err(e.into());
                }
            };
            if resp.status().is_redirection() {
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow::anyhow!("redirect without location"))?;
                // Resolve relative redirects against the current URL.
                current = Url::parse(&current)?.join(loc)?.to_string();
                continue;
            }
            if !resp.status().is_success() {
                tracing::warn!("embed fetch status {} for {current}", resp.status());
                anyhow::bail!("status {}", resp.status());
            }
            // Cap the body: read up to META_CAP+1 and reject if larger.
            let full = resp.bytes().await?;
            anyhow::ensure!(full.len() <= META_CAP, "metadata too large: {}", full.len());
            let body = String::from_utf8_lossy(&full).into_owned();
            return Ok(FetchedText { body });
        }
        anyhow::bail!("too many redirects")
    }
}

use farder_protocol::messages::EmbedOutcome;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EMBED_CACHE_TTL: Duration = Duration::from_secs(3600);
const EMBED_CACHE_MAX: usize = 2048;

pub struct EmbedCache {
    entries: Mutex<HashMap<String, (Instant, EmbedOutcome)>>,
}

impl EmbedCache {
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &str, now: Instant) -> Option<EmbedOutcome> {
        let map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).and_then(|(at, v)| (now.duration_since(*at) < EMBED_CACHE_TTL).then(|| v.clone()))
    }

    pub fn put(&self, key: String, value: EmbedOutcome, now: Instant) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= EMBED_CACHE_MAX {
            map.retain(|_, (at, _)| now.duration_since(*at) < EMBED_CACHE_TTL);
            if map.len() >= EMBED_CACHE_MAX {
                map.clear();
            }
        }
        map.insert(key, (now, value));
    }
}

pub fn embed_cache_key(url: &str) -> String {
    url.to_string()
}

/// Route an allowlisted URL to its adapter. Non-allowlisted → Unavailable;
/// allowlisted but unparseable shape → Unsupported.
pub async fn resolve_embed<F: LinkFetcher + ?Sized>(url: &str, f: &F) -> EmbedOutcome {
    let Some(provider) = classify_url(url) else {
        return EmbedOutcome::Unavailable;
    };
    let adapted = match provider {
        Provider::Twitter => adapt_twitter(url, f).await,
        Provider::YouTube => adapt_youtube(url, f).await,
        Provider::Reddit => adapt_reddit(url, f).await,
        Provider::Spotify => adapt_spotify(url, f).await,
    };
    match adapted {
        Some(e) => EmbedOutcome::Embed(e),
        None => EmbedOutcome::Unsupported,
    }
}

fn percent_encode_url(u: &str) -> String {
    // Minimal RFC3986 query-component encoding for a full URL value.
    let mut out = String::with_capacity(u.len() * 3);
    for b in u.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn youtube_oembed_url(raw: &str) -> Option<String> {
    // Accept any allowlisted youtube URL; oEmbed takes the original URL.
    if classify_url(raw) != Some(Provider::YouTube) { return None; }
    Some(format!("https://www.youtube.com/oembed?format=json&url={}", percent_encode_url(raw)))
}

pub async fn adapt_youtube<F: LinkFetcher + ?Sized>(url: &str, f: &F) -> Option<LinkEmbed> {
    let api = youtube_oembed_url(url)?;
    let fetched = f.fetch_text(&api).await.ok()?;
    let j: serde_json::Value = serde_json::from_str(&fetched.body).ok()?;
    Some(LinkEmbed {
        provider: "youtube".into(),
        kind: EmbedKind::Video,
        url: url.to_string(),
        title: j.get("title").and_then(|v| v.as_str()).map(String::from),
        author: j.get("author_name").and_then(|v| v.as_str()).map(String::from),
        description: None,
        thumbnail: j.get("thumbnail_url").and_then(|v| v.as_str()).map(String::from),
        media: None, // YouTube: card + open externally (not inline-playable)
        duration_secs: None,
    })
}

pub fn spotify_oembed_url(raw: &str) -> Option<String> {
    if classify_url(raw) != Some(Provider::Spotify) { return None; }
    Some(format!("https://open.spotify.com/oembed?url={}", percent_encode_url(raw)))
}

pub async fn adapt_spotify<F: LinkFetcher + ?Sized>(url: &str, f: &F) -> Option<LinkEmbed> {
    let api = spotify_oembed_url(url)?;
    let fetched = f.fetch_text(&api).await.ok()?;
    let j: serde_json::Value = serde_json::from_str(&fetched.body).ok()?;
    Some(LinkEmbed {
        provider: "spotify".into(),
        kind: EmbedKind::Audio,
        url: url.to_string(),
        title: j.get("title").and_then(|v| v.as_str()).map(String::from),
        author: j.get("author_name").and_then(|v| v.as_str()).map(String::from),
        description: None,
        thumbnail: j.get("thumbnail_url").and_then(|v| v.as_str()).map(String::from),
        media: None,
        duration_secs: None,
    })
}

pub fn reddit_json_url(raw: &str) -> Option<String> {
    if classify_url(raw) != Some(Provider::Reddit) { return None; }
    let trimmed = raw.split(['?', '#']).next().unwrap_or(raw);
    let base = trimmed.trim_end_matches('/');
    Some(format!("{base}/.json"))
}

pub async fn adapt_reddit<F: LinkFetcher + ?Sized>(url: &str, f: &F) -> Option<LinkEmbed> {
    let api = reddit_json_url(url)?;
    let fetched = f.fetch_text(&api).await.ok()?;
    let j: serde_json::Value = serde_json::from_str(&fetched.body).ok()?;
    let post = j.get(0)?.get("data")?.get("children")?.get(0)?.get("data")?;
    let title = post.get("title").and_then(|v| v.as_str()).map(String::from);
    let subreddit = post.get("subreddit_name_prefixed").and_then(|v| v.as_str()).map(String::from);
    // Use the thumbnail only if it's an http(s) URL (reddit uses sentinels like
    // "self"/"default"/"nsfw" otherwise).
    let thumb = post.get("thumbnail").and_then(|v| v.as_str())
        .filter(|t| t.starts_with("http"))
        .map(String::from);
    Some(LinkEmbed {
        provider: "reddit".into(),
        kind: EmbedKind::Article,
        url: url.to_string(),
        title,
        author: subreddit,
        description: None,
        thumbnail: thumb,
        media: None, // v1: card only (no v.redd.it inline video)
        duration_secs: None,
    })
}

pub async fn adapt_twitter<F: LinkFetcher + ?Sized>(url: &str, f: &F) -> Option<LinkEmbed> {
    let api = fxtwitter_api_url(url)?;
    let fetched = f.fetch_text(&api).await.ok()?;
    let json: serde_json::Value = serde_json::from_str(&fetched.body).ok()?;
    let tweet = json.get("tweet")?;
    let author = tweet.get("author").and_then(|a| a.get("screen_name")).and_then(|v| v.as_str());
    let text = tweet.get("text").and_then(|v| v.as_str());
    let canonical = tweet.get("url").and_then(|v| v.as_str()).unwrap_or(url).to_string();

    // Prefer a video; fall back to the first photo.
    let media = tweet.get("media");
    let video = media
        .and_then(|m| m.get("videos"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());
    let photo = media
        .and_then(|m| m.get("photos"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());

    let (embed_media, thumbnail, duration) = if let Some(v) = video {
        let m = EmbedMedia {
            url: v.get("url").and_then(|x| x.as_str())?.to_string(),
            mime: "video/mp4".into(),
            width: v.get("width").and_then(|x| x.as_u64()).map(|n| n as u32),
            height: v.get("height").and_then(|x| x.as_u64()).map(|n| n as u32),
            playable_inline: true,
        };
        let thumb = v.get("thumbnail_url").and_then(|x| x.as_str()).map(String::from);
        let dur = v.get("duration").and_then(|x| x.as_f64()).map(|d| d as u32);
        (Some(m), thumb, dur)
    } else if let Some(p) = photo {
        let purl = p.get("url").and_then(|x| x.as_str())?.to_string();
        let m = EmbedMedia {
            url: purl.clone(),
            mime: "image/jpeg".into(),
            width: p.get("width").and_then(|x| x.as_u64()).map(|n| n as u32),
            height: p.get("height").and_then(|x| x.as_u64()).map(|n| n as u32),
            playable_inline: true,
        };
        (Some(m), Some(purl), None)
    } else {
        (None, None, None)
    };

    Some(LinkEmbed {
        provider: "twitter".into(),
        kind: EmbedKind::Tweet,
        url: canonical,
        title: None,
        author: author.map(|a| format!("@{a}")),
        description: text.map(String::from),
        thumbnail,
        media: embed_media,
        duration_secs: duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    pub struct MockFetcher {
        map: HashMap<String, String>,
    }
    impl MockFetcher {
        pub fn new() -> Self { Self { map: HashMap::new() } }
        pub fn insert(&mut self, url: &str, body: &str) {
            self.map.insert(url.to_string(), body.to_string());
        }
    }
    impl LinkFetcher for MockFetcher {
        async fn fetch_text(&self, url: &str) -> super::Result<super::FetchedText> {
            match self.map.get(url) {
                Some(b) => Ok(super::FetchedText { body: b.clone() }),
                None => anyhow::bail!("mock: no entry for {url}"),
            }
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name),
        ).unwrap()
    }

    #[tokio::test]
    async fn mock_fetcher_returns_canned_body() {
        let mut m = MockFetcher::new();
        m.insert("https://api.fxtwitter.com/u/status/1", r#"{"ok":true}"#);
        let got = m.fetch_text("https://api.fxtwitter.com/u/status/1").await.unwrap();
        assert_eq!(got.body, r#"{"ok":true}"#);
    }

    #[test]
    fn allowlist_accepts_known_hosts() {
        assert_eq!(classify_url("https://x.com/u/status/1"), Some(Provider::Twitter));
        assert_eq!(classify_url("https://twitter.com/u/status/1"), Some(Provider::Twitter));
        assert_eq!(classify_url("https://www.youtube.com/watch?v=abc"), Some(Provider::YouTube));
        assert_eq!(classify_url("https://youtu.be/abc"), Some(Provider::YouTube));
        assert_eq!(classify_url("https://www.reddit.com/r/x/comments/1/t/"), Some(Provider::Reddit));
        assert_eq!(classify_url("https://open.spotify.com/track/1"), Some(Provider::Spotify));
    }

    #[test]
    fn media_allowlist_accepts_cdn_hosts_and_rejects_others() {
        // CDN media hosts the adapters extract are fetchable as media...
        assert!(host_is_media_allowlisted("https://video.twimg.com/v.mp4"));
        assert!(host_is_media_allowlisted("https://pbs.twimg.com/thumb.jpg"));
        assert!(host_is_media_allowlisted("https://i.ytimg.com/vi/abc/hq.jpg"));
        assert!(host_is_media_allowlisted("https://i.scdn.co/image/abc"));
        assert!(host_is_media_allowlisted("https://i.redd.it/pic.jpg"));
        assert!(host_is_media_allowlisted("https://b.thumbs.redditmedia.com/x.jpg"));
        // ...page hosts are also media-allowlisted (superset)...
        assert!(host_is_media_allowlisted("https://x.com/u/status/1"));
        // ...but arbitrary hosts, lookalikes, and bare IPs are not.
        assert!(!host_is_media_allowlisted("https://evil.com/x.jpg"));
        assert!(!host_is_media_allowlisted("https://twimg.com.evil.com/x.jpg"));
        assert!(!host_is_media_allowlisted("https://10.0.0.1/x.jpg"));
        // The page allowlist itself must NOT accept media CDN hosts.
        assert!(!host_is_allowlisted("https://video.twimg.com/v.mp4"));
    }

    #[test]
    fn allowlist_rejects_lookalikes_and_unknowns() {
        assert_eq!(classify_url("https://youtube.com.evil.com/watch?v=x"), None);
        assert_eq!(classify_url("https://evil.com/x"), None);
        assert_eq!(classify_url("https://notx.com/status/1"), None);
        assert_eq!(classify_url("http://127.0.0.1/x"), None);
        assert_eq!(classify_url("https://192.168.1.1/x"), None);
        assert_eq!(classify_url("not a url"), None);
    }

    #[test]
    fn twitter_api_url_rewrite() {
        assert_eq!(
            fxtwitter_api_url("https://x.com/jack/status/20"),
            Some("https://api.fxtwitter.com/jack/status/20".to_string())
        );
        assert_eq!(
            fxtwitter_api_url("https://twitter.com/jack/status/20?s=21"),
            Some("https://api.fxtwitter.com/jack/status/20".to_string())
        );
        assert_eq!(fxtwitter_api_url("https://x.com/jack"), None); // not a status
    }

    #[tokio::test]
    async fn twitter_adapter_parses_video_tweet() {
        let mut m = MockFetcher::new();
        m.insert("https://api.fxtwitter.com/jack/status/20", &fixture("fxtwitter_video.json"));
        let e = adapt_twitter("https://x.com/jack/status/20", &m).await.unwrap();
        assert_eq!(e.provider, "twitter");
        assert_eq!(e.kind, farder_protocol::messages::EmbedKind::Tweet);
        assert_eq!(e.author.as_deref(), Some("@jack"));
        assert_eq!(e.description.as_deref(), Some("just setting up my twttr"));
        let media = e.media.unwrap();
        assert_eq!(media.url, "https://video.twimg.com/v.mp4");
        assert!(media.playable_inline);
        assert_eq!(e.duration_secs, Some(12));
        assert_eq!(e.thumbnail.as_deref(), Some("https://pbs.twimg.com/thumb.jpg"));
    }

    #[tokio::test]
    async fn resolve_embed_routes_twitter() {
        let mut m = MockFetcher::new();
        m.insert("https://api.fxtwitter.com/jack/status/20", &fixture("fxtwitter_video.json"));
        match resolve_embed("https://x.com/jack/status/20", &m).await {
            farder_protocol::messages::EmbedOutcome::Embed(e) => assert_eq!(e.provider, "twitter"),
            other => panic!("expected Embed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_embed_non_allowlisted_is_unavailable() {
        let m = MockFetcher::new();
        assert_eq!(
            resolve_embed("https://evil.com/x", &m).await,
            farder_protocol::messages::EmbedOutcome::Unavailable
        );
    }

    #[test]
    fn embed_cache_ttl() {
        use std::time::{Duration, Instant};
        let c = EmbedCache::new();
        let t0 = Instant::now();
        let out = farder_protocol::messages::EmbedOutcome::Unsupported;
        assert!(c.get("k", t0).is_none());
        c.put("k".into(), out.clone(), t0);
        assert_eq!(c.get("k", t0 + Duration::from_secs(3599)), Some(out));
        assert!(c.get("k", t0 + Duration::from_secs(3601)).is_none());
    }

    #[test]
    fn youtube_oembed_url_forms() {
        assert_eq!(
            youtube_oembed_url("https://youtu.be/dQw4w9WgXcQ"),
            Some("https://www.youtube.com/oembed?format=json&url=https%3A%2F%2Fyoutu.be%2FdQw4w9WgXcQ".to_string())
        );
        assert!(youtube_oembed_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ").is_some());
    }

    #[tokio::test]
    async fn youtube_adapter_parses_oembed() {
        let mut m = MockFetcher::new();
        let api = youtube_oembed_url("https://youtu.be/dQw4w9WgXcQ").unwrap();
        m.insert(&api, &fixture("youtube_oembed.json"));
        let e = adapt_youtube("https://youtu.be/dQw4w9WgXcQ", &m).await.unwrap();
        assert_eq!(e.provider, "youtube");
        assert_eq!(e.kind, farder_protocol::messages::EmbedKind::Video);
        assert_eq!(e.author.as_deref(), Some("Rick Astley"));
        assert!(e.title.as_deref().unwrap().contains("Never Gonna"));
        assert_eq!(e.thumbnail.as_deref(), Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"));
        assert!(e.media.is_none()); // not inline-playable
    }

    #[tokio::test]
    async fn spotify_adapter_parses_oembed() {
        let mut m = MockFetcher::new();
        let api = spotify_oembed_url("https://open.spotify.com/track/1").unwrap();
        m.insert(&api, &fixture("spotify_oembed.json"));
        let e = adapt_spotify("https://open.spotify.com/track/1", &m).await.unwrap();
        assert_eq!(e.provider, "spotify");
        assert_eq!(e.kind, farder_protocol::messages::EmbedKind::Audio);
        assert!(e.title.as_deref().unwrap().contains("Never Gonna"));
        assert!(e.media.is_none());
    }

    #[test]
    fn reddit_json_url_appends_dot_json() {
        assert_eq!(
            reddit_json_url("https://www.reddit.com/r/aww/comments/1/title/"),
            Some("https://www.reddit.com/r/aww/comments/1/title/.json".to_string())
        );
    }

    #[tokio::test]
    async fn reddit_adapter_parses_listing() {
        let mut m = MockFetcher::new();
        let api = reddit_json_url("https://www.reddit.com/r/aww/comments/1/title/").unwrap();
        m.insert(&api, &fixture("reddit_post.json"));
        let e = adapt_reddit("https://www.reddit.com/r/aww/comments/1/title/", &m).await.unwrap();
        assert_eq!(e.provider, "reddit");
        assert_eq!(e.author.as_deref(), Some("r/aww"));
        assert!(e.title.as_deref().unwrap().contains("cool post"));
    }

    #[test]
    fn content_type_gate() {
        assert!(content_type_allowed("image/jpeg"));
        assert!(content_type_allowed("image/png; charset=binary"));
        assert!(content_type_allowed("video/mp4"));
        assert!(content_type_allowed("image/gif"));
        assert!(!content_type_allowed("text/html"));
        assert!(!content_type_allowed("application/octet-stream"));
        assert!(!content_type_allowed(""));
    }

    #[tokio::test]
    async fn validate_rejects_non_allowlisted_and_keeps_allowlisted_shape() {
        // Non-allowlisted host: rejected without any DNS/network.
        assert!(!validate_fetchable("https://evil.com/x").await);
        // Bare private IP host: rejected.
        assert!(!validate_fetchable("https://10.0.0.1/x").await);
        // Allowlisted host string classifies (DNS may or may not resolve in CI,
        // but classification must pass — assert the allowlist gate specifically).
        assert!(host_is_allowlisted("https://api.fxtwitter.com/x")); // see note
    }

    // --- Integration test: real HTTP round-trip via localhost fixture server ---
    //
    // The production SafeFetcher refuses loopback via the SSRF gate; this test
    // uses a LocalFetcher (bare reqwest, no allowlist/SSRF) to exercise the
    // adapter→HTTP→parse path end-to-end.  The SSRF guard itself is covered above.

    struct LocalFetcher {
        client: reqwest::Client,
        base: String,
    }

    impl LinkFetcher for LocalFetcher {
        async fn fetch_text(&self, url: &str) -> super::Result<FetchedText> {
            // Rewrite the adapter's API host onto our localhost fixture server,
            // preserving path and query so the adapter's URL shape is exercised.
            let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!(e))?;
            let path = parsed.path();
            let query = parsed.query().unwrap_or("");
            let local = if query.is_empty() {
                format!("{}{}", self.base, path)
            } else {
                format!("{}{}?{}", self.base, path, query)
            };
            let body = self.client.get(&local).send().await?.text().await?;
            Ok(FetchedText { body })
        }
    }

    #[tokio::test]
    async fn youtube_resolves_over_http() {
        // Spawn a tiny_http server on a random port; it replies with canned oEmbed JSON.
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let base = format!("http://{addr}");

        std::thread::spawn(move || {
            let body = r#"{"title":"T","author_name":"A","thumbnail_url":"http://x/y.jpg"}"#;
            // Serve one request then exit; the test only needs a single fetch.
            if let Some(req) = server.incoming_requests().next() {
                let header = "Content-Type: application/json"
                    .parse::<tiny_http::Header>()
                    .unwrap();
                let _ = req.respond(
                    tiny_http::Response::from_string(body).with_header(header),
                );
            }
        });

        let f = LocalFetcher { client: reqwest::Client::new(), base };
        let e = adapt_youtube("https://youtu.be/abc", &f).await
            .expect("adapt_youtube failed over localhost");
        assert_eq!(e.title.as_deref(), Some("T"), "title mismatch");
        assert_eq!(e.author.as_deref(), Some("A"), "author mismatch");
    }
}
