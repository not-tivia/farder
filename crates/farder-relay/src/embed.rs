//! Rich external link embeds — the relay's fetch proxy phase two. Resolves
//! allowlisted URLs to normalized `LinkEmbed` metadata and streams media bytes,
//! so a requester's IP never touches the third-party site.

use anyhow::Result;
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
}
