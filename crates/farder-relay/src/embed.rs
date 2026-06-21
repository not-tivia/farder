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
    Image,
}

/// Allowlist entries: (registrable-or-exact host suffix, provider). A URL host
/// matches an entry if it equals the suffix OR ends with "." + suffix (so
/// subdomains match, but `youtube.com.evil.com` does NOT match `youtube.com`).
const ALLOWLIST: &[(&str, Provider)] = &[
    ("twitter.com", Provider::Twitter),
    ("x.com", Provider::Twitter),
    ("youtube.com", Provider::YouTube),
    ("youtu.be", Provider::YouTube),
    ("reddit.com", Provider::Reddit),
    ("redd.it", Provider::Reddit),
    ("open.spotify.com", Provider::Spotify),
    // Curated direct-image hosts (v1).
    ("i.redd.it", Provider::Image),
    ("i.imgur.com", Provider::Image),
];

/// True if a host matches an allowlist suffix safely (exact or dotted-subdomain).
fn host_matches(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// Classify a URL to a provider, or None if its host is not allowlisted.
/// `i.redd.it` (Image) is checked before `redd.it` (Reddit) via longest-match.
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
    // Longest suffix first so i.redd.it (Image) wins over redd.it (Reddit).
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

/// True if the URL's host is allowlisted at all (used to re-validate media URLs).
pub fn host_is_allowlisted(raw: &str) -> bool {
    classify_url(raw).is_some()
}

/// A successfully fetched text document and the final URL after any redirects.
pub struct FetchedText {
    pub body: String,
    pub final_url: String,
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
        Provider::Image => adapt_image(url).await,
    };
    match adapted {
        Some(e) => EmbedOutcome::Embed(e),
        None => EmbedOutcome::Unsupported,
    }
}

pub async fn adapt_youtube<F: LinkFetcher + ?Sized>(_u: &str, _f: &F) -> Option<LinkEmbed> { None }
pub async fn adapt_reddit<F: LinkFetcher + ?Sized>(_u: &str, _f: &F) -> Option<LinkEmbed> { None }
pub async fn adapt_spotify<F: LinkFetcher + ?Sized>(_u: &str, _f: &F) -> Option<LinkEmbed> { None }
pub async fn adapt_image(_u: &str) -> Option<LinkEmbed> { None }

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
                Some(b) => Ok(super::FetchedText { body: b.clone(), final_url: url.to_string() }),
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
        assert_eq!(classify_url("https://i.redd.it/abc.jpg"), Some(Provider::Image));
        assert_eq!(classify_url("https://i.imgur.com/abc.png"), Some(Provider::Image));
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
}
