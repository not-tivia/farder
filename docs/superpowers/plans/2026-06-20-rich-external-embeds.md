# Rich External Link Embeds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render curated rich embeds (Twitter/X, YouTube, direct images, Reddit, Spotify) for links posted in chat, with all fetching done by the relay so a viewer's IP never reaches the third-party site.

**Architecture:** Extend the relay-as-privacy-fetch-proxy (already proven by `ProxyInvitePreview`) with two new request types: `ProxyLinkEmbed { url }` returns normalized metadata (`LinkEmbed`); `ProxyMedia { url }` streams a capped media/thumbnail byte stream. The client detects allowlisted URLs in message text, fetches metadata via the default relay, renders a per-provider card, and pulls thumbnails/inline-video bytes through the relay as blob URLs. Hybrid video: inline player for direct-file media, card+external-browser for YouTube.

**Tech Stack:** Rust (farder-protocol, farder-relay), `reqwest` (rustls-tls) as the new relay HTTP client, Quinn/QUIC transport, React + TypeScript + Tauri client.

## Global Constraints

- **Curated allowlist only.** Only allowlisted hosts are ever fetched. Generic OpenGraph / arbitrary image hosts are explicitly out of scope.
- **SSRF guard is mandatory on every outbound fetch**, including media URLs extracted from pages and every redirect hop. Reuse `farder_relay::proxy::is_global_ip`.
- **No `Date.now()`/`Math.random()` constraints do not apply** (not a workflow script) — normal Rust/TS.
- **Frontend↔backend seam has zero drift:** every `invoke("X")` name must have a matching `#[tauri::command] fn X` AND an entry in `generate_handler!` in `client/src-tauri/src/main.rs`.
- **UI must be styled in ALL THREE themes** (`client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css`) using `var(--xp-…)` variables — never hard-coded colors. A `className` with no CSS in every theme is a bug.
- **Docs updated in the same commit** as the surface they describe (tauri-commands.md, tauri-bridge.md, protocol module doc, a new relay-embed module doc, ARCHITECTURE.md).
- **Uniform failure:** all failure modes (SSRF refusal, timeout, non-allowlisted, parse failure, rate-limit) collapse to `EmbedOutcome::Unavailable`; recognized-but-unhandled URL shapes → `EmbedOutcome::Unsupported`. Never leak *why*.
- **Ops reality:** this changes the relay binary (adds HTTP fetching) → requires a VPS relay redeploy AND a client rebuild before runtime verification. Headless tests prove parsing/guards/transport; the end-to-end render is UNVERIFIED until an owner Windows run.
- Run the full gate before declaring done: `cargo test --workspace` (from repo root `/home/deez/farder`), `cd client/src-tauri && cargo test`, `cd client && npx tsc --noEmit`.

---

## File Structure

**farder-protocol:**
- Modify `crates/farder-protocol/src/messages.rs` — add `EmbedKind`, `EmbedMedia`, `LinkEmbed`, `EmbedOutcome` types; add `ProxyLinkEmbed`, `ProxyLinkEmbedResult`, `ProxyMedia`, `ProxyMediaChunk`/`ProxyMediaError` to the `Message` enum; round-trip tests.

**farder-relay:**
- Create `crates/farder-relay/src/embed.rs` — allowlist/host classification, the `LinkFetcher` seam, the production `SafeFetcher` (reqwest + SSRF + caps), per-provider adapters, `resolve_embed`, the embed TTL cache, and the media-proxy streaming logic.
- Modify `crates/farder-relay/src/router.rs` — add `EmbedContext` (cache + limiter + fetcher), dispatch `ProxyLinkEmbed` → `handle_link_embed`, `ProxyMedia` → `handle_media`.
- Modify `crates/farder-relay/src/main.rs` — build the `EmbedContext`, thread it into `serve`/`handle_connection`.
- Modify `crates/farder-relay/Cargo.toml` — add `reqwest`, `serde_json`, `url`, `futures-util` (for streaming); add fixture files under `tests/`.
- Create `crates/farder-relay/tests/fixtures/` — canned fxtwitter JSON, YouTube oEmbed JSON, Spotify oEmbed JSON, Reddit JSON.

**client (Rust):**
- Modify `client/src-tauri/src/commands.rs` — `get_link_embed(url)` and `get_proxied_media(url)` commands + a session cache.
- Modify `client/src-tauri/src/main.rs` — register both in `generate_handler!`.

**client (TS):**
- Create `client/src/lib/linkEmbed.ts` — allowlist URL detection regex + `detectEmbedUrls(text)`.
- Create `client/src/hooks/useLinkEmbed.ts` — fetch + cache hook (mirrors `useInvitePreview`).
- Create `client/src/hooks/useProxiedMedia.ts` — fetch bytes → blob URL, revoke on unmount.
- Create `client/src/components/LinkEmbed.tsx` — the per-provider card.
- Modify `client/src/components/Message.tsx` — render detected link embeds (mirror the existing invite-embed block).
- Modify `client/src/lib/tauri-bridge.ts` — `getLinkEmbed`, `getProxiedMedia`.
- Modify `client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css` — embed card classes.
- Modify the settings store + a settings UI component — the data-saver toggle.

**docs:**
- Create `docs/modules/relay-embed.md`; modify `docs/modules/tauri-commands.md`, `docs/modules/tauri-bridge.md`, the protocol module doc, `ARCHITECTURE.md`.

---

## PHASE 1 — Protocol

### Task 1: Embed types + protocol messages

**Files:**
- Modify: `crates/farder-protocol/src/messages.rs`
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `EmbedKind`, `EmbedMedia`, `LinkEmbed`, `EmbedOutcome`, and `Message::{ProxyLinkEmbed, ProxyLinkEmbedResult, ProxyMedia, ProxyMediaHeader, ProxyMediaUnavailable}`. The relay (Tasks 5–12) and client (Tasks 13–14) consume these.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `messages.rs`:

```rust
#[test]
fn test_roundtrip_link_embed() {
    let embed = LinkEmbed {
        provider: "twitter".into(),
        kind: EmbedKind::Tweet,
        url: "https://x.com/a/status/1".into(),
        title: Some("hi".into()),
        author: Some("@a".into()),
        description: Some("body".into()),
        thumbnail: Some("https://pbs.example/t.jpg".into()),
        media: Some(EmbedMedia {
            url: "https://video.example/v.mp4".into(),
            mime: "video/mp4".into(),
            width: Some(640),
            height: Some(360),
            playable_inline: true,
        }),
        duration_secs: Some(12),
    };
    for outcome in [
        EmbedOutcome::Embed(embed.clone()),
        EmbedOutcome::Unsupported,
        EmbedOutcome::Unavailable,
    ] {
        let msg = Message::ProxyLinkEmbedResult { outcome: outcome.clone() };
        let bytes = codec::encode(&msg).unwrap();
        match codec::decode::<Message>(&bytes).unwrap() {
            Message::ProxyLinkEmbedResult { outcome: o } => assert_eq!(o, outcome),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    let req = Message::ProxyLinkEmbed { url: "https://x.com/a/status/1".into() };
    assert!(matches!(
        codec::decode::<Message>(&codec::encode(&req).unwrap()).unwrap(),
        Message::ProxyLinkEmbed { .. }
    ));

    let media = Message::ProxyMedia { url: "https://video.example/v.mp4".into() };
    assert!(matches!(
        codec::decode::<Message>(&codec::encode(&media).unwrap()).unwrap(),
        Message::ProxyMedia { .. }
    ));

    let hdr = Message::ProxyMediaHeader { content_type: "image/jpeg".into(), total_len: 1024 };
    assert!(matches!(
        codec::decode::<Message>(&codec::encode(&hdr).unwrap()).unwrap(),
        Message::ProxyMediaHeader { .. }
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-protocol test_roundtrip_link_embed`
Expected: FAIL — `EmbedKind`/`LinkEmbed`/variants not found (compile error).

- [ ] **Step 3: Write minimal implementation**

Add near the top of `messages.rs` (after the existing `PreviewOutcome`):

```rust
/// Coarse class of an external embed, used by the client to pick a card layout.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum EmbedKind {
    Tweet,
    Video,
    Image,
    Audio,
    Article,
}

/// A directly-fetchable media asset (image or direct video file) the client
/// renders inline by pulling its bytes via `ProxyMedia`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EmbedMedia {
    pub url: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// true for direct-file media playable in a `<video>`/`<img>`; false for
    /// sources (YouTube, Spotify) that must open in an external browser.
    pub playable_inline: bool,
}

/// Normalized metadata for one external link, produced by a relay-side adapter.
/// The client never sees raw HTML; only this struct.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LinkEmbed {
    pub provider: String,
    pub kind: EmbedKind,
    pub url: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    /// URL of a thumbnail/preview image, fetched via `ProxyMedia`.
    pub thumbnail: Option<String>,
    pub media: Option<EmbedMedia>,
    pub duration_secs: Option<u32>,
}

/// Result of a `ProxyLinkEmbed` lookup. Uniform failure leaks nothing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum EmbedOutcome {
    Embed(LinkEmbed),
    /// URL host is allowlisted but the specific URL shape isn't handled.
    Unsupported,
    /// Timeout, SSRF refusal, non-allowlisted host, rate-limit, parse failure.
    Unavailable,
}
```

Then add to the `Message` enum (after `ProxyInvitePreviewResult`):

```rust
    /// Ask the relay to resolve a rich embed for an external URL (relay fetch
    /// proxy, phase two). First message on a fresh connection.
    ProxyLinkEmbed { url: String },
    /// The relay's normalized answer to `ProxyLinkEmbed`.
    ProxyLinkEmbedResult { outcome: EmbedOutcome },
    /// Ask the relay to stream a media/thumbnail asset (image or direct video)
    /// on the requester's behalf. First message on a fresh connection.
    ProxyMedia { url: String },
    /// Sent by the relay before the raw media bytes: the validated content type
    /// and total length. Followed by length-framed raw chunks on the stream.
    ProxyMediaHeader { content_type: String, total_len: u64 },
    /// Sent by the relay instead of a header when the media can't be served
    /// (non-allowlisted, SSRF refusal, over cap, bad content-type, timeout).
    ProxyMediaUnavailable,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-protocol`
Expected: PASS (all protocol tests, including the new one).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-protocol/src/messages.rs
git commit -m "protocol: add LinkEmbed types + ProxyLinkEmbed/ProxyMedia messages"
```

---

## PHASE 2 — Relay embed resolver core

### Task 2: Relay HTTP dependencies

**Files:**
- Modify: `crates/farder-relay/Cargo.toml`

**Interfaces:**
- Produces: `reqwest`, `serde_json`, `url`, `futures-util` available to the relay crate.

- [ ] **Step 1: Add dependencies**

In `crates/farder-relay/Cargo.toml` under `[dependencies]`, add:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
serde_json = "1"
url = "2"
futures-util = "0.3"
```

(Note: `default-features = false` + `rustls-tls` keeps us off OpenSSL/native-tls, matching the rustls stack already used by quinn.)

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p farder-relay`
Expected: builds (downloads reqwest et al. on first run).

- [ ] **Step 3: Commit**

```bash
git add crates/farder-relay/Cargo.toml Cargo.lock
git commit -m "relay: add reqwest/serde_json/url deps for embed fetch proxy"
```

---

### Task 3: Host allowlist + provider classification

**Files:**
- Create: `crates/farder-relay/src/embed.rs`
- Modify: `crates/farder-relay/src/main.rs` (add `mod embed;`)
- Test: in `embed.rs`

**Interfaces:**
- Produces: `enum Provider { Twitter, YouTube, Reddit, Spotify, Image }`, `fn classify_url(url: &str) -> Option<Provider>`, `fn host_is_allowlisted(url: &str) -> bool`. Consumed by `resolve_embed` (Task 6) and media re-validation (Task 9).

- [ ] **Step 1: Write the failing test**

Create `crates/farder-relay/src/embed.rs` with only:

```rust
//! Rich external link embeds — the relay's fetch proxy phase two. Resolves
//! allowlisted URLs to normalized `LinkEmbed` metadata and streams media bytes,
//! so a requester's IP never touches the third-party site.

#[cfg(test)]
mod tests {
    use super::*;

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-relay allowlist_`
Expected: FAIL — `classify_url`/`Provider` not defined.

- [ ] **Step 3: Write minimal implementation**

Add above the test module in `embed.rs`:

```rust
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
```

Add to `crates/farder-relay/src/main.rs` near the other `mod` lines:

```rust
mod embed;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-relay allowlist_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-relay/src/embed.rs crates/farder-relay/src/main.rs
git commit -m "relay: embed host allowlist + provider classification"
```

---

### Task 4: The LinkFetcher seam + mock fetcher

**Files:**
- Modify: `crates/farder-relay/src/embed.rs`
- Test: in `embed.rs`

**Interfaces:**
- Produces: `struct FetchedText { body: String, final_url: String }`, `trait LinkFetcher { async fn fetch_text(&self, url: &str) -> anyhow::Result<FetchedText>; }`, and a test-only `MockFetcher`. Adapters (Tasks 5, 7, 8) consume `&dyn LinkFetcher`. The production `SafeFetcher` (Task 9) implements it.

- [ ] **Step 1: Write the failing test**

Add to `embed.rs` test module:

```rust
#[tokio::test]
async fn mock_fetcher_returns_canned_body() {
    let mut m = MockFetcher::new();
    m.insert("https://api.fxtwitter.com/u/status/1", r#"{"ok":true}"#);
    let got = m.fetch_text("https://api.fxtwitter.com/u/status/1").await.unwrap();
    assert_eq!(got.body, r#"{"ok":true}"#);
}
```

Add `tokio` test support: `farder-relay` already depends on `tokio`; ensure `#[tokio::test]` works (it does with the `tokio` macros feature in the workspace dep).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-relay mock_fetcher_`
Expected: FAIL — `LinkFetcher`/`MockFetcher` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `embed.rs` (above tests):

```rust
use anyhow::Result;

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
```

Add inside the `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-relay mock_fetcher_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-relay/src/embed.rs
git commit -m "relay: LinkFetcher seam + mock fetcher for adapter tests"
```

---

### Task 5: Twitter/X adapter (fxtwitter) — the first end-to-end adapter

**Files:**
- Modify: `crates/farder-relay/src/embed.rs`
- Create: `crates/farder-relay/tests/fixtures/fxtwitter_video.json`
- Test: in `embed.rs`

**Interfaces:**
- Consumes: `LinkFetcher`, `Provider`, protocol `LinkEmbed`/`EmbedMedia`/`EmbedKind`.
- Produces: `async fn adapt_twitter(url: &str, f: &dyn LinkFetcher) -> Option<LinkEmbed>`, `fn fxtwitter_api_url(url: &str) -> Option<String>`.

- [ ] **Step 1: Create the fixture**

Create `crates/farder-relay/tests/fixtures/fxtwitter_video.json` (trimmed real fxtwitter shape):

```json
{
  "code": 200,
  "tweet": {
    "url": "https://x.com/jack/status/20",
    "text": "just setting up my twttr",
    "author": { "name": "jack", "screen_name": "jack" },
    "media": {
      "videos": [
        { "url": "https://video.twimg.com/v.mp4", "type": "video",
          "width": 640, "height": 360, "duration": 12.5,
          "thumbnail_url": "https://pbs.twimg.com/thumb.jpg" }
      ]
    }
  }
}
```

- [ ] **Step 2: Write the failing test**

Add to the `embed.rs` test module:

```rust
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name),
        ).unwrap()
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p farder-relay twitter_`
Expected: FAIL — `fxtwitter_api_url`/`adapt_twitter` not defined.

- [ ] **Step 4: Write minimal implementation**

Add to `embed.rs`:

```rust
use farder_protocol::messages::{EmbedKind, EmbedMedia, LinkEmbed};

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

pub async fn adapt_twitter(url: &str, f: &dyn LinkFetcher) -> Option<LinkEmbed> {
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p farder-relay twitter_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/embed.rs crates/farder-relay/tests/fixtures/fxtwitter_video.json
git commit -m "relay: Twitter/X embed adapter via fxtwitter JSON"
```

---

### Task 6: resolve_embed dispatcher + embed cache

**Files:**
- Modify: `crates/farder-relay/src/embed.rs`
- Test: in `embed.rs`

**Interfaces:**
- Consumes: `classify_url`, `adapt_twitter` (other adapters wired in Tasks 7–8), `LinkFetcher`.
- Produces: `async fn resolve_embed(url: &str, f: &dyn LinkFetcher) -> EmbedOutcome`, `struct EmbedCache` (TTL 1h, mirrors `proxy::PreviewCache`), `fn embed_cache_key(url: &str) -> String`.

- [ ] **Step 1: Write the failing test**

Add to `embed.rs` test module:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-relay resolve_embed embed_cache_ttl`
Expected: FAIL — `resolve_embed`/`EmbedCache` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `embed.rs`:

```rust
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
    pub fn new() -> Self { Self { entries: Mutex::new(HashMap::new()) } }
    pub fn get(&self, key: &str, now: Instant) -> Option<EmbedOutcome> {
        let map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).and_then(|(at, v)| (now.duration_since(*at) < EMBED_CACHE_TTL).then(|| v.clone()))
    }
    pub fn put(&self, key: String, value: EmbedOutcome, now: Instant) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= EMBED_CACHE_MAX {
            map.retain(|_, (at, _)| now.duration_since(*at) < EMBED_CACHE_TTL);
            if map.len() >= EMBED_CACHE_MAX { map.clear(); }
        }
        map.insert(key, (now, value));
    }
}

pub fn embed_cache_key(url: &str) -> String { url.to_string() }

/// Route an allowlisted URL to its adapter. Non-allowlisted → Unavailable;
/// allowlisted but unparseable shape → Unsupported.
pub async fn resolve_embed(url: &str, f: &dyn LinkFetcher) -> EmbedOutcome {
    let Some(provider) = classify_url(url) else { return EmbedOutcome::Unavailable; };
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
```

> NOTE: this references `adapt_youtube`, `adapt_reddit`, `adapt_spotify`, `adapt_image` defined in Tasks 7–8. To keep this task compiling on its own, temporarily add minimal stubs returning `None` and replace them in Tasks 7–8:
>
> ```rust
> pub async fn adapt_youtube(_u: &str, _f: &dyn LinkFetcher) -> Option<LinkEmbed> { None }
> pub async fn adapt_reddit(_u: &str, _f: &dyn LinkFetcher) -> Option<LinkEmbed> { None }
> pub async fn adapt_spotify(_u: &str, _f: &dyn LinkFetcher) -> Option<LinkEmbed> { None }
> pub async fn adapt_image(_u: &str) -> Option<LinkEmbed> { None }
> ```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-relay resolve_embed embed_cache_ttl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-relay/src/embed.rs
git commit -m "relay: resolve_embed dispatcher + 1h embed TTL cache"
```

---

### Task 7: YouTube + Spotify adapters (oEmbed)

**Files:**
- Modify: `crates/farder-relay/src/embed.rs` (replace the YouTube/Spotify stubs)
- Create: `crates/farder-relay/tests/fixtures/youtube_oembed.json`, `crates/farder-relay/tests/fixtures/spotify_oembed.json`
- Test: in `embed.rs`

**Interfaces:**
- Produces: real `adapt_youtube`, `adapt_spotify`; `fn youtube_oembed_url(url) -> Option<String>`, `fn spotify_oembed_url(url) -> Option<String>`.

- [ ] **Step 1: Create fixtures**

`tests/fixtures/youtube_oembed.json`:

```json
{ "title": "Rick Astley - Never Gonna Give You Up",
  "author_name": "Rick Astley",
  "thumbnail_url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg" }
```

`tests/fixtures/spotify_oembed.json`:

```json
{ "title": "Never Gonna Give You Up",
  "thumbnail_url": "https://i.scdn.co/image/abc",
  "provider_name": "Spotify" }
```

- [ ] **Step 2: Write the failing test**

```rust
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p farder-relay youtube_ spotify_`
Expected: FAIL (stubs return None / functions missing).

- [ ] **Step 4: Write minimal implementation**

Replace the YouTube/Spotify stubs in `embed.rs` with:

```rust
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

pub async fn adapt_youtube(url: &str, f: &dyn LinkFetcher) -> Option<LinkEmbed> {
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

pub async fn adapt_spotify(url: &str, f: &dyn LinkFetcher) -> Option<LinkEmbed> {
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p farder-relay youtube_ spotify_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/embed.rs crates/farder-relay/tests/fixtures/youtube_oembed.json crates/farder-relay/tests/fixtures/spotify_oembed.json
git commit -m "relay: YouTube + Spotify oEmbed adapters"
```

---

### Task 8: Reddit + direct-image adapters

**Files:**
- Modify: `crates/farder-relay/src/embed.rs` (replace Reddit/image stubs)
- Create: `crates/farder-relay/tests/fixtures/reddit_post.json`
- Test: in `embed.rs`

**Interfaces:**
- Produces: real `adapt_reddit`, `adapt_image`; `fn reddit_json_url(url) -> Option<String>`.

- [ ] **Step 1: Create fixture**

`tests/fixtures/reddit_post.json` (trimmed Reddit listing shape — array of listings):

```json
[
  { "data": { "children": [
    { "data": {
      "title": "A cool post",
      "subreddit_name_prefixed": "r/aww",
      "thumbnail": "https://b.thumbs.redditmedia.com/x.jpg",
      "url": "https://i.redd.it/pic.jpg"
    } }
  ] } }
]
```

- [ ] **Step 2: Write the failing test**

```rust
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

    #[tokio::test]
    async fn image_adapter_builds_image_embed() {
        let e = adapt_image("https://i.redd.it/pic.jpg").await.unwrap();
        assert_eq!(e.provider, "image");
        assert_eq!(e.kind, farder_protocol::messages::EmbedKind::Image);
        let media = e.media.unwrap();
        assert_eq!(media.url, "https://i.redd.it/pic.jpg");
        assert!(media.playable_inline);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p farder-relay reddit_ image_adapter_`
Expected: FAIL.

- [ ] **Step 4: Write minimal implementation**

Replace the Reddit/image stubs:

```rust
pub fn reddit_json_url(raw: &str) -> Option<String> {
    if classify_url(raw) != Some(Provider::Reddit) { return None; }
    let trimmed = raw.split(['?', '#']).next().unwrap_or(raw);
    let base = trimmed.trim_end_matches('/');
    Some(format!("{base}/.json"))
}

pub async fn adapt_reddit(url: &str, f: &dyn LinkFetcher) -> Option<LinkEmbed> {
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

/// Direct image: no fetch needed to build the embed; the bytes come later via
/// ProxyMedia (which validates content-type). The host is already allowlisted.
pub async fn adapt_image(url: &str) -> Option<LinkEmbed> {
    Some(LinkEmbed {
        provider: "image".into(),
        kind: EmbedKind::Image,
        url: url.to_string(),
        title: None,
        author: None,
        description: None,
        thumbnail: Some(url.to_string()),
        media: Some(EmbedMedia {
            url: url.to_string(),
            mime: "image/*".into(),
            width: None,
            height: None,
            playable_inline: true,
        }),
        duration_secs: None,
    })
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p farder-relay reddit_ image_adapter_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/embed.rs crates/farder-relay/tests/fixtures/reddit_post.json
git commit -m "relay: Reddit + direct-image embed adapters"
```

---

## PHASE 3 — Production fetcher + media proxy

### Task 9: SafeFetcher (production HTTP with allowlist + SSRF + caps)

**Files:**
- Modify: `crates/farder-relay/src/embed.rs`
- Test: in `embed.rs` (SSRF/cap logic that needs no network)

**Interfaces:**
- Consumes: `host_is_allowlisted`, `proxy::is_global_ip`.
- Produces: `struct SafeFetcher { client: reqwest::Client }`, `impl LinkFetcher for SafeFetcher`, `async fn validate_fetchable(url: &str) -> bool` (allowlist + DNS-resolve + is_global_ip on every resolved IP), const `META_CAP: usize = 16 * 1024`.

- [ ] **Step 1: Write the failing test**

```rust
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
```

> NOTE: `api.fxtwitter.com` must be added to the allowlist as a Twitter host (the adapter fetches it). Update `ALLOWLIST` in Task 3's array to also include `("fxtwitter.com", Provider::Twitter)` — `api.fxtwitter.com` then matches via the dotted-subdomain rule. Add this entry now and re-run Task 3's tests (still green).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-relay validate_rejects_`
Expected: FAIL — `validate_fetchable`/`SafeFetcher` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `embed.rs`:

```rust
use crate::proxy::is_global_ip;

pub const META_CAP: usize = 16 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Allowlist + SSRF gate: the host must be allowlisted AND every IP it resolves
/// to must be globally routable. Used before BOTH page fetches and media fetches
/// (re-validates adapter-extracted media URLs — the "media-URL trap").
pub async fn validate_fetchable(raw: &str) -> bool {
    if !host_is_allowlisted(raw) { return false; }
    let Ok(u) = Url::parse(raw) else { return false; };
    let Some(host) = u.host_str() else { return false; };
    let port = u.port_or_known_default().unwrap_or(443);
    // Resolve and require ALL resolved addresses to be global (anti-rebind).
    let Ok(addrs) = tokio::net::lookup_host((host, port)).await else { return false; };
    let mut any = false;
    for a in addrs {
        any = true;
        if !is_global_ip(a.ip()) { return false; }
    }
    any
}

pub struct SafeFetcher {
    client: reqwest::Client,
}
impl SafeFetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none()) // we handle hops ourselves
            .user_agent("FarderRelay/1.0 (+https://farder.gg)")
            .build()?;
        Ok(Self { client })
    }
}

impl LinkFetcher for SafeFetcher {
    async fn fetch_text(&self, url: &str) -> Result<FetchedText> {
        // Manual redirect loop (max 3) with SSRF re-validation on every hop.
        let mut current = url.to_string();
        for _ in 0..4 {
            if !validate_fetchable(&current).await {
                anyhow::bail!("fetch refused (allowlist/ssrf): {current}");
            }
            let resp = self.client.get(&current).send().await?;
            if resp.status().is_redirection() {
                let loc = resp.headers().get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow::anyhow!("redirect without location"))?;
                // Resolve relative redirects against the current URL.
                current = Url::parse(&current)?.join(loc)?.to_string();
                continue;
            }
            anyhow::ensure!(resp.status().is_success(), "status {}", resp.status());
            // Cap the body: read up to META_CAP+1 and reject if larger.
            let full = resp.bytes().await?;
            anyhow::ensure!(full.len() <= META_CAP, "metadata too large: {}", full.len());
            let body = String::from_utf8_lossy(&full).into_owned();
            return Ok(FetchedText { body, final_url: current });
        }
        anyhow::bail!("too many redirects")
    }
}
```

> NOTE on the body cap: `resp.bytes()` buffers the whole body; for metadata (JSON/oEmbed) that's fine under `META_CAP`. The streaming byte-counted cap matters for *media* (Task 10), which uses a streaming reader, not this path.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-relay validate_rejects_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-relay/src/embed.rs
git commit -m "relay: SafeFetcher (allowlist + SSRF + redirect re-validation + meta cap)"
```

---

### Task 10: Media proxy fetch (streaming, capped, content-type gated)

**Files:**
- Modify: `crates/farder-relay/src/embed.rs`
- Test: in `embed.rs`

**Interfaces:**
- Produces: `const MEDIA_CAP: u64 = 25 * 1024 * 1024;`, `fn content_type_allowed(ct: &str) -> bool`, `async fn fetch_media(client: &reqwest::Client, url: &str) -> Result<(String, Vec<u8>)>` returning `(content_type, bytes)` (validated host, capped during read).

- [ ] **Step 1: Write the failing test**

```rust
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
```

(The networked `fetch_media` is exercised by the integration test in Task 12 against a localhost fixture server; here we test the pure content-type gate.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-relay content_type_gate`
Expected: FAIL — `content_type_allowed` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
use futures_util::StreamExt;

pub const MEDIA_CAP: u64 = 25 * 1024 * 1024;

pub fn content_type_allowed(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    base.starts_with("image/") || base == "video/mp4"
}

/// Fetch a media asset with full guards: allowlist + SSRF re-validation,
/// content-type allowlist, and a byte-counted cap enforced DURING streaming
/// (Content-Length is not trusted). Returns (content_type, bytes).
pub async fn fetch_media(client: &reqwest::Client, url: &str) -> Result<(String, Vec<u8>)> {
    if !validate_fetchable(url).await {
        anyhow::bail!("media refused (allowlist/ssrf): {url}");
    }
    let resp = client.get(url).send().await?;
    anyhow::ensure!(resp.status().is_success(), "media status {}", resp.status());
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    anyhow::ensure!(content_type_allowed(&ct), "media content-type rejected: {ct}");

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
```

> NOTE: `fetch_media` takes the `reqwest::Client` directly (it follows NO redirects via the SafeFetcher policy; if a media URL redirects, it fails closed — acceptable for v1; thumbnails/direct media are stable URLs). Reuse `SafeFetcher`'s client by exposing `pub fn client(&self) -> &reqwest::Client`.

Add to `impl SafeFetcher`:

```rust
    pub fn client(&self) -> &reqwest::Client { &self.client }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p farder-relay content_type_gate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-relay/src/embed.rs
git commit -m "relay: media proxy fetch (streaming cap + content-type gate + SSRF)"
```

---

## PHASE 4 — Relay dispatch wiring

### Task 11: EmbedContext + router dispatch for ProxyLinkEmbed and ProxyMedia

**Files:**
- Modify: `crates/farder-relay/src/router.rs`
- Modify: `crates/farder-relay/src/main.rs`
- Test: protocol-level dispatch covered by Task 12 integration; this task wires it and must compile + keep existing tests green.

**Interfaces:**
- Consumes: `embed::{EmbedCache, SafeFetcher, resolve_embed, fetch_media, embed_cache_key}`, `limits::ConnectionLimiter`.
- Produces: `struct EmbedContext { cache: EmbedCache, limiter: ConnectionLimiter, fetcher: Arc<SafeFetcher> }`, `fn new_embed_context() -> Result<Arc<EmbedContext>>`, dispatch arms in `handle_connection`, `async fn handle_link_embed(...)`, `async fn handle_media(...)`.

- [ ] **Step 1: Add EmbedContext and constructor**

In `router.rs`, after `new_preview_context`:

```rust
/// Everything the embed proxy needs at dispatch time.
pub struct EmbedContext {
    pub cache: crate::embed::EmbedCache,
    pub limiter: crate::limits::ConnectionLimiter,
    pub media_limiter: crate::limits::ConnectionLimiter,
    pub fetcher: std::sync::Arc<crate::embed::SafeFetcher>,
}

pub fn new_embed_context() -> Result<Arc<EmbedContext>> {
    Ok(Arc::new(EmbedContext {
        cache: crate::embed::EmbedCache::new(),
        // 30 metadata previews/min/IP (same posture as invite previews).
        limiter: crate::limits::ConnectionLimiter::new(usize::MAX, 30, std::time::Duration::from_secs(60)),
        // Separate, tighter bucket for bandwidth-heavy media fetches.
        media_limiter: crate::limits::ConnectionLimiter::new(usize::MAX, 60, std::time::Duration::from_secs(60)),
        fetcher: std::sync::Arc::new(crate::embed::SafeFetcher::new()?),
    }))
}
```

- [ ] **Step 2: Thread EmbedContext through handle_connection + serve**

Change `handle_connection` and `serve` signatures to also take `embed: Arc<EmbedContext>` (alongside `preview: Arc<PreviewContext>`), and add dispatch arms in the `match msg` in `handle_connection`:

```rust
        Message::ProxyLinkEmbed { url } => {
            handle_link_embed(url, conn, send, embed).await
        }
        Message::ProxyMedia { url } => {
            handle_media(url, conn, send, embed).await
        }
```

Update the call site in `main.rs` to build `let embed = router::new_embed_context()?;` and pass it into `serve(...)` (and wherever `handle_connection` is invoked from the accept loop). Mirror exactly how `preview` is already passed.

- [ ] **Step 3: Implement handle_link_embed**

In `router.rs`:

```rust
/// Answer a ProxyLinkEmbed: rate-limit → cache → resolve (8s budget) → reply.
async fn handle_link_embed(
    url: String,
    client_conn: Connection,
    mut send: SendStream,
    embed: Arc<EmbedContext>,
) -> Result<()> {
    use farder_protocol::messages::EmbedOutcome;
    let ip = client_conn.remote_address().ip();
    let now = std::time::Instant::now();

    let outcome = if url.len() > 2048 {
        EmbedOutcome::Unavailable
    } else if embed.limiter.try_admit(ip, now).is_none() {
        EmbedOutcome::Unavailable
    } else {
        let key = crate::embed::embed_cache_key(&url);
        match embed.cache.get(&key, now) {
            Some(hit) => hit,
            None => {
                let fresh = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    crate::embed::resolve_embed(&url, embed.fetcher.as_ref()),
                ).await.unwrap_or(EmbedOutcome::Unavailable);
                embed.cache.put(key, fresh.clone(), std::time::Instant::now());
                fresh
            }
        }
    };

    let reply = codec::encode(&Message::ProxyLinkEmbedResult { outcome })?;
    write_message(&mut send, &reply).await?;
    let _ = send.finish();
    client_conn.closed().await;
    Ok(())
}
```

- [ ] **Step 4: Implement handle_media**

```rust
/// Answer a ProxyMedia: rate-limit → fetch (validated, capped) → reply a
/// ProxyMediaHeader then the raw bytes length-framed, or ProxyMediaUnavailable.
async fn handle_media(
    url: String,
    client_conn: Connection,
    mut send: SendStream,
    embed: Arc<EmbedContext>,
) -> Result<()> {
    let ip = client_conn.remote_address().ip();
    let now = std::time::Instant::now();

    let result = if url.len() > 2048 || embed.media_limiter.try_admit(ip, now).is_none() {
        None
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::embed::fetch_media(embed.fetcher.client(), &url),
        ).await.ok().and_then(|r| r.ok())
    };

    match result {
        Some((content_type, bytes)) => {
            let hdr = codec::encode(&Message::ProxyMediaHeader {
                content_type,
                total_len: bytes.len() as u64,
            })?;
            write_message(&mut send, &hdr).await?;
            // Raw bytes follow, length-framed (4-byte BE len + bytes).
            send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
            send.write_all(&bytes).await?;
        }
        None => {
            let msg = codec::encode(&Message::ProxyMediaUnavailable)?;
            write_message(&mut send, &msg).await?;
        }
    }
    let _ = send.finish();
    client_conn.closed().await;
    Ok(())
}
```

- [ ] **Step 5: Build + run existing relay tests**

Run: `cargo test -p farder-relay`
Expected: PASS (all existing + new unit tests; dispatch compiles).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/router.rs crates/farder-relay/src/main.rs
git commit -m "relay: dispatch ProxyLinkEmbed + ProxyMedia (EmbedContext, rate-limited, cached)"
```

---

### Task 12: Relay integration test (localhost fixture HTTP server)

**Files:**
- Create: `crates/farder-relay/tests/embed_proxy.rs`
- Modify: `crates/farder-relay/Cargo.toml` (`[dev-dependencies]`: add a tiny HTTP server — use `tiny_http = "0.12"`)

**Interfaces:**
- Consumes: `farder_relay::embed::{resolve_embed, fetch_media, SafeFetcher, LinkFetcher}`.
- This is where the production fetch path is exercised end-to-end against a real (localhost) HTTP server. Because `validate_fetchable` refuses loopback, the test injects a **test fetcher that points at localhost with the SSRF gate bypassed** — proving adapter→HTTP→parse works, while the production guard stays intact and is unit-tested separately (Task 9).

- [ ] **Step 1: Add dev-dependency**

In `crates/farder-relay/Cargo.toml` `[dev-dependencies]`: `tiny_http = "0.12"`.

- [ ] **Step 2: Write the integration test**

Create `crates/farder-relay/tests/embed_proxy.rs`:

```rust
//! Exercises the production HTTP fetch path against a localhost fixture server.
//! The SafeFetcher's SSRF guard refuses loopback by design, so this test uses a
//! LocalFetcher (reqwest with no allowlist/SSRF) to prove adapter→HTTP→parse;
//! the guard itself is unit-tested in embed.rs.

use farder_relay::embed::{FetchedText, LinkFetcher};

struct LocalFetcher { client: reqwest::Client, base: String }

impl LinkFetcher for LocalFetcher {
    async fn fetch_text(&self, url: &str) -> anyhow::Result<FetchedText> {
        // Rewrite the adapter's api host to our localhost fixture server.
        let path = reqwest::Url::parse(url).unwrap();
        let local = format!("{}{}?{}", self.base, path.path(), path.query().unwrap_or(""));
        let body = self.client.get(&local).send().await?.text().await?;
        Ok(FetchedText { body, final_url: url.to_string() })
    }
}

#[tokio::test]
async fn youtube_resolves_over_http() {
    // Spawn a localhost fixture server returning oEmbed JSON.
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr();
    let base = format!("http://{}", addr);
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let body = r#"{"title":"T","author_name":"A","thumbnail_url":"http://x/y.jpg"}"#;
            let _ = req.respond(tiny_http::Response::from_string(body)
                .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap()));
        }
    });

    let f = LocalFetcher { client: reqwest::Client::new(), base };
    let e = farder_relay::embed::adapt_youtube("https://youtu.be/abc", &f).await.unwrap();
    assert_eq!(e.title.as_deref(), Some("T"));
    assert_eq!(e.author.as_deref(), Some("A"));
}
```

> NOTE: this requires `farder-relay` to expose `embed` publicly to integration tests. Add `pub mod embed;` visibility — but `farder-relay` is a binary crate (`main.rs`). To allow `tests/` to import it, add a `src/lib.rs` that re-exports the modules (`pub mod embed; pub mod proxy; pub mod limits; ...`) and have `main.rs` use the lib. If that restructure is too large for this task, instead move the integration assertions into `embed.rs`'s `#[cfg(test)] mod tests` using the localhost server inline. **Pick the lib.rs route** if `main.rs` already cleanly separates modules; otherwise inline.

- [ ] **Step 3: Run the test**

Run: `cargo test -p farder-relay --test embed_proxy`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/farder-relay/tests/embed_proxy.rs crates/farder-relay/Cargo.toml Cargo.lock
git commit -m "relay: integration test for embed fetch over localhost HTTP"
```

---

## PHASE 5 — Client backend

### Task 13: get_link_embed command

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs` (register in `generate_handler!`)
- Test: a Rust unit test for the cache helper in `commands.rs`

**Interfaces:**
- Consumes: `default_relay::default_relay()`, `tls::make_pinned_relay_endpoint`, `connection::{write_frame, read_frame}`, protocol `Message::{ProxyLinkEmbed, ProxyLinkEmbedResult}`, `EmbedOutcome`.
- Produces: `#[tauri::command] async fn get_link_embed(url: String) -> Result<EmbedOutcome, String>` (registered as `get_link_embed`).

- [ ] **Step 1: Write the failing test**

Add to `commands.rs` tests (or create a `#[cfg(test)]` block):

```rust
#[test]
fn link_embed_cache_roundtrip() {
    use farder_protocol::messages::EmbedOutcome;
    let c = link_embed_cache();
    {
        let mut m = c.lock().unwrap();
        m.insert("u".into(), (std::time::Instant::now(), EmbedOutcome::Unsupported));
    }
    let hit = {
        let m = c.lock().unwrap();
        m.get("u").map(|(_, v)| v.clone())
    };
    assert_eq!(hit, Some(EmbedOutcome::Unsupported));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client/src-tauri && cargo test link_embed_cache_roundtrip`
Expected: FAIL — `link_embed_cache` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `commands.rs` (mirroring `fetch_preview_via_relay` / `get_invite_preview`):

```rust
use farder_protocol::messages::EmbedOutcome;

static LINK_EMBED_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, EmbedOutcome)>>,
> = std::sync::OnceLock::new();

fn link_embed_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, EmbedOutcome)>> {
    LINK_EMBED_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve a rich embed for an external URL through the default relay. Throwaway
/// connection; never touches session connections. LAZY ONLY (PIN-lock rule):
/// needs no identity — embeds are anonymous.
#[tauri::command]
pub async fn get_link_embed(url: String) -> Result<EmbedOutcome, String> {
    use farder_protocol::messages::Message;

    // 5-minute client cache (embeds are stable; relay caches 1h).
    {
        let cache = link_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, hit)) = cache.get(&url) {
            if at.elapsed() < std::time::Duration::from_secs(300) {
                return Ok(hit.clone());
            }
        }
    }

    let Some((relay_addr, relay_fp)) = crate::default_relay::default_relay() else {
        return Ok(EmbedOutcome::Unavailable);
    };
    let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
    let conn = endpoint.connect(relay_addr, "farder-relay")
        .map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let msg = farder_protocol::codec::encode(&Message::ProxyLinkEmbed { url: url.clone() })
        .map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &msg).await.map_err(|e| e.to_string())?;
    let reply_bytes = crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?;
    let reply: Message = farder_protocol::codec::decode(&reply_bytes).map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"embed done");

    let outcome = match reply {
        Message::ProxyLinkEmbedResult { outcome } => outcome,
        other => return Err(format!("unexpected relay reply: {:?}", other)),
    };
    {
        let mut cache = link_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(url, (std::time::Instant::now(), outcome.clone()));
    }
    Ok(outcome)
}
```

Register in `client/src-tauri/src/main.rs` `generate_handler!` list: add `commands::get_link_embed,`.

- [ ] **Step 4: Run test + build**

Run: `cd client/src-tauri && cargo test link_embed_cache_roundtrip && cargo build`
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "client: get_link_embed command (relay embed proxy, cached)"
```

---

### Task 14: get_proxied_media command

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`
- Test: build/seam (the byte path is exercised at runtime)

**Interfaces:**
- Produces: `#[tauri::command] async fn get_proxied_media(url: String) -> Result<ProxiedMedia, String>` where `ProxiedMedia { content_type: String, data_base64: String }` (base64 so the bytes cross the Tauri IPC boundary cleanly; JS decodes to a Blob).

- [ ] **Step 1: Write the implementation**

Add to `commands.rs`:

```rust
#[derive(serde::Serialize)]
pub struct ProxiedMedia {
    pub content_type: String,
    pub data_base64: String,
}

/// Pull a media asset (thumbnail or direct video) through the default relay and
/// return it base64-encoded for the webview to wrap in a Blob URL. The webview
/// never fetches the CDN directly (IP-leak protection).
#[tauri::command]
pub async fn get_proxied_media(url: String) -> Result<ProxiedMedia, String> {
    use farder_protocol::messages::Message;
    use base64::Engine;

    let Some((relay_addr, relay_fp)) = crate::default_relay::default_relay() else {
        return Err("no default relay".into());
    };
    let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
    let conn = endpoint.connect(relay_addr, "farder-relay")
        .map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let msg = farder_protocol::codec::encode(&Message::ProxyMedia { url }).map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &msg).await.map_err(|e| e.to_string())?;

    // First frame: header or unavailable.
    let hdr_bytes = crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?;
    let hdr: Message = farder_protocol::codec::decode(&hdr_bytes).map_err(|e| e.to_string())?;
    let (content_type, total_len) = match hdr {
        Message::ProxyMediaHeader { content_type, total_len } => (content_type, total_len),
        Message::ProxyMediaUnavailable => { conn.close(0u32.into(), b"done"); return Err("media unavailable".into()); }
        other => { conn.close(0u32.into(), b"done"); return Err(format!("unexpected: {:?}", other)); }
    };
    // Then the raw length-framed bytes.
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len as u64 != total_len { conn.close(0u32.into(), b"done"); return Err("length mismatch".into()); }
    let mut data = vec![0u8; len];
    recv.read_exact(&mut data).await.map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"media done");

    Ok(ProxiedMedia {
        content_type,
        data_base64: base64::engine::general_purpose::STANDARD.encode(&data),
    })
}
```

> NOTE: confirm `base64` is a client-crate dependency (it is used elsewhere for inline data URLs). If not, add `base64 = "0.22"` to `client/src-tauri/Cargo.toml`. Also confirm `recv.read_exact` is available (Quinn `RecvStream` supports `read_exact` via the `tokio::io::AsyncReadExt`-style API used in `proxy.rs::read_capped`); mirror that exact call style.

Register in `main.rs`: `commands::get_proxied_media,`.

- [ ] **Step 2: Build + seam audit**

Run: `cd client/src-tauri && cargo build`
Then verify the seam: `grep -n "get_link_embed\|get_proxied_media" client/src-tauri/src/main.rs` (both must appear in `generate_handler!`).
Expected: builds; both names present.

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src-tauri/Cargo.toml Cargo.lock
git commit -m "client: get_proxied_media command (relay media stream -> base64)"
```

---

### Task 15: Bridge functions + URL detection lib

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`
- Create: `client/src/lib/linkEmbed.ts`
- Test: a TS-level reasoning check via tsc (no JS test runner) + a small self-contained assertion file run with `node` is NOT available; rely on tsc + the regex being simple. Add a `linkEmbed.test-notes` comment block enumerating cases that the reviewer manually verifies.

**Interfaces:**
- Produces: `getLinkEmbed(url): Promise<EmbedOutcome>`, `getProxiedMedia(url): Promise<{content_type:string; data_base64:string}>` in the bridge; `detectEmbedUrls(text: string): string[]` and the TS `EmbedOutcome`/`LinkEmbed` types in `linkEmbed.ts`.

- [ ] **Step 1: Add bridge functions**

In `client/src/lib/tauri-bridge.ts`:

```ts
import type { EmbedOutcome } from "./linkEmbed";

export function getLinkEmbed(url: string): Promise<EmbedOutcome> {
  return invoke<EmbedOutcome>("get_link_embed", { url });
}

export function getProxiedMedia(url: string): Promise<{ content_type: string; data_base64: string }> {
  return invoke<{ content_type: string; data_base64: string }>("get_proxied_media", { url });
}
```

- [ ] **Step 2: Create linkEmbed.ts (types + detection)**

```ts
// Mirrors farder_protocol::messages embed types (serde external tagging).
export type EmbedKind = "Tweet" | "Video" | "Image" | "Audio" | "Article";

export interface EmbedMedia {
  url: string;
  mime: string;
  width: number | null;
  height: number | null;
  playable_inline: boolean;
}

export interface LinkEmbed {
  provider: string;
  kind: EmbedKind;
  url: string;
  title: string | null;
  author: string | null;
  description: string | null;
  thumbnail: string | null;
  media: EmbedMedia | null;
  duration_secs: number | null;
}

// serde serializes `EmbedOutcome::Embed(x)` as { Embed: x }, and the unit
// variants as the bare strings "Unsupported" / "Unavailable".
export type EmbedOutcome = { Embed: LinkEmbed } | "Unsupported" | "Unavailable";

// Allowlist host detection — MUST match the relay's ALLOWLIST (embed.rs).
const ALLOWLIST_HOSTS = [
  "twitter.com", "x.com", "youtube.com", "youtu.be",
  "reddit.com", "redd.it", "open.spotify.com", "i.redd.it", "i.imgur.com",
];

function hostAllowed(host: string): boolean {
  const h = host.toLowerCase();
  return ALLOWLIST_HOSTS.some((s) => h === s || h.endsWith("." + s));
}

const URL_RE = /https?:\/\/[^\s<>"']+/g;

/** Extract up to 3 unique allowlisted URLs from message text. */
export function detectEmbedUrls(text: string): string[] {
  const found: string[] = [];
  const seen = new Set<string>();
  for (const m of text.match(URL_RE) ?? []) {
    if (found.length >= 3) break;
    let host: string;
    try { host = new URL(m).host; } catch { continue; }
    if (!hostAllowed(host)) continue;
    if (seen.has(m)) continue;
    seen.add(m);
    found.push(m);
  }
  return found;
}
```

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add client/src/lib/tauri-bridge.ts client/src/lib/linkEmbed.ts
git commit -m "client: embed bridge fns + allowlisted URL detection"
```

---

## PHASE 6 — Client UI

### Task 16: useLinkEmbed + useProxiedMedia hooks

**Files:**
- Create: `client/src/hooks/useLinkEmbed.ts`
- Create: `client/src/hooks/useProxiedMedia.ts`

**Interfaces:**
- Consumes: `getLinkEmbed`, `getProxiedMedia`, `LinkEmbed`, `EmbedOutcome`.
- Produces: `useLinkEmbed(url, enabled): { status, embed }` and `useProxiedMedia(url, enabled): string | null` (blob URL, revoked on unmount/url-change).

- [ ] **Step 1: useLinkEmbed**

```ts
import { useEffect, useState } from "react";
import { getLinkEmbed } from "../lib/tauri-bridge";
import type { LinkEmbed } from "../lib/linkEmbed";

type State =
  | { status: "loading"; embed: null }
  | { status: "ok"; embed: LinkEmbed }
  | { status: "unsupported" | "unavailable"; embed: null };

export function useLinkEmbed(url: string, enabled: boolean): State {
  const [state, setState] = useState<State>({ status: "loading", embed: null });
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    setState({ status: "loading", embed: null });
    getLinkEmbed(url)
      .then((out) => {
        if (!alive) return;
        if (typeof out === "object" && "Embed" in out) setState({ status: "ok", embed: out.Embed });
        else if (out === "Unsupported") setState({ status: "unsupported", embed: null });
        else setState({ status: "unavailable", embed: null });
      })
      .catch(() => { if (alive) setState({ status: "unavailable", embed: null }); });
    return () => { alive = false; };
  }, [url, enabled]);
  return state;
}
```

- [ ] **Step 2: useProxiedMedia**

```ts
import { useEffect, useState } from "react";
import { getProxiedMedia } from "../lib/tauri-bridge";

/** Fetch media bytes via the relay and expose a blob URL; revokes on cleanup. */
export function useProxiedMedia(url: string | null, enabled: boolean): string | null {
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!url || !enabled) { setBlobUrl(null); return; }
    let alive = true;
    let created: string | null = null;
    getProxiedMedia(url)
      .then(({ content_type, data_base64 }) => {
        if (!alive) return;
        const bytes = Uint8Array.from(atob(data_base64), (c) => c.charCodeAt(0));
        const blob = new Blob([bytes], { type: content_type });
        created = URL.createObjectURL(blob);
        setBlobUrl(created);
      })
      .catch(() => { if (alive) setBlobUrl(null); });
    return () => { alive = false; if (created) URL.revokeObjectURL(created); };
  }, [url, enabled]);
  return blobUrl;
}
```

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add client/src/hooks/useLinkEmbed.ts client/src/hooks/useProxiedMedia.ts
git commit -m "client: useLinkEmbed + useProxiedMedia (blob-url) hooks"
```

---

### Task 17: LinkEmbed card component

**Files:**
- Create: `client/src/components/LinkEmbed.tsx`

**Interfaces:**
- Consumes: `useLinkEmbed`, `useProxiedMedia`, the external-open helper used by invite links (find it: `grep -rn "openUrl\|shell.*open\|opener" client/src` — reuse the same call the YouTube "Play" needs; commonly `@tauri-apps/plugin-opener`'s `openUrl` or an existing wrapper).
- Produces: `export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean })`.

- [ ] **Step 1: Implement the component**

```tsx
import { useState } from "react";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
// Reuse the project's existing external-open helper (confirm import path).
import { openExternal } from "../lib/external"; // adjust to the real helper

export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  // Data-saver: don't auto-load; show a chip that loads on click.
  const [loaded, setLoaded] = useState(!dataSaver);
  const state = useLinkEmbed(url, loaded);

  if (!loaded) {
    return (
      <button className="link-embed-chip" onClick={() => setLoaded(true)}>
        Load preview
      </button>
    );
  }
  if (state.status === "loading") return <div className="link-embed link-embed-state">Loading preview&hellip;</div>;
  if (state.status !== "ok") return null; // unsupported/unavailable: render nothing extra
  const e = state.embed;

  const inlineMedia = e.media && e.media.playable_inline ? e.media : null;
  const isVideo = inlineMedia?.mime.startsWith("video/");
  // Inline media bytes load now (card is showing); for a video this is the file.
  const mediaBlob = useProxiedMedia(inlineMedia?.url ?? null, !!inlineMedia);
  const thumbBlob = useProxiedMedia(e.thumbnail ?? null, !inlineMedia && !!e.thumbnail);

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {inlineMedia && isVideo && mediaBlob && (
        <video className="link-embed-video" src={mediaBlob} controls preload="metadata" />
      )}
      {inlineMedia && !isVideo && mediaBlob && (
        <img className="link-embed-image" src={mediaBlob} alt={e.title ?? ""} />
      )}
      {!inlineMedia && thumbBlob && (
        <div className="link-embed-thumb-wrap">
          <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button className="link-embed-play" onClick={() => openExternal(e.url)}>
              &#9654; {e.kind === "Video" ? "Play" : "Open"}
            </button>
          )}
        </div>
      )}
      {e.duration_secs != null && (
        <div className="link-embed-duration">{formatDuration(e.duration_secs)}</div>
      )}
    </div>
  );
}

function formatDuration(s: number): string {
  const m = Math.floor(s / 60);
  const sec = String(s % 60).padStart(2, "0");
  return `${m}:${sec}`;
}
```

> NOTE: `useProxiedMedia` is called unconditionally per React's rules-of-hooks — the two calls above are both at the top level of the component body (not inside conditionals); the `enabled` flag gates the fetch. Keep them before any early `return`. **Reorder if needed so both hook calls run on every render** (move them above the `if (!loaded)` / `if (state.status...)` returns, passing `enabled: loaded && state.status === "ok" && ...`). Confirm with tsc + the rules-of-hooks lint.

- [ ] **Step 2: Resolve the external-open + hooks-order details, type-check**

Find the real external-open helper and fix the import. Reorder the two `useProxiedMedia` calls above the early returns. Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/LinkEmbed.tsx
git commit -m "client: LinkEmbed card (inline video/image, thumbnail+open, data-saver chip)"
```

---

### Task 18: Wire LinkEmbed into Message.tsx

**Files:**
- Modify: `client/src/components/Message.tsx`

**Interfaces:**
- Consumes: `detectEmbedUrls`, `LinkEmbed`, the data-saver setting (Task 20 provides it; until then pass `false`).

- [ ] **Step 1: Add the embed block**

After the existing invite-embeds IIFE block (around line 478, after the `</div>` of `invite-embeds`), add:

```tsx
      {!deleted && (() => {
        const urls = detectEmbedUrls(message.content);
        return urls.length > 0 ? (
          <div className="link-embeds">
            {urls.map((u, i) => <LinkEmbed key={i} url={u} dataSaver={dataSaver} />)}
          </div>
        ) : null;
      })()}
```

Add imports at the top: `import LinkEmbed from "./LinkEmbed";` and `import { detectEmbedUrls } from "../lib/linkEmbed";`. For `dataSaver`, read it from settings (Task 20); until Task 20 lands, define `const dataSaver = false;` locally and replace it in Task 20.

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/Message.tsx
git commit -m "client: render link embeds below message text"
```

---

### Task 19: Theme CSS for embed cards (all 3 themes)

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

**Interfaces:**
- Produces: CSS for `.link-embeds`, `.link-embed`, `.link-embed--twitter/youtube/reddit/spotify/image`, `.link-embed-author/title/desc/duration`, `.link-embed-video/image/thumb/thumb-wrap/play`, `.link-embed-chip`, `.link-embed-state`.

- [ ] **Step 1: Add CSS to each theme**

For EACH of the three theme files, add a block using that theme's existing variables (do not hard-code colors). Example using the vars seen in the existing `.invite-embed` rules — adapt variable names to each theme:

```css
.link-embeds { display: flex; flex-direction: column; gap: 6px; margin-top: 6px; }
.link-embed {
  border: 1px solid var(--xp-border);
  border-left: 3px solid var(--xp-blue);
  background: var(--xp-panel-bg);
  border-radius: 6px;
  padding: 8px 10px;
  max-width: 460px;
}
.link-embed-author { font-weight: 600; color: var(--xp-text-normal); font-size: 0.85em; }
.link-embed-title { font-weight: 600; color: var(--xp-text-normal); margin: 2px 0; }
.link-embed-desc { color: var(--xp-text-secondary); font-size: 0.9em; white-space: pre-wrap; }
.link-embed-video, .link-embed-image, .link-embed-thumb {
  max-width: 100%; border-radius: 4px; margin-top: 6px; display: block;
}
.link-embed-thumb-wrap { position: relative; display: inline-block; }
.link-embed-play {
  position: absolute; inset: 0; margin: auto; width: fit-content; height: fit-content;
  padding: 6px 12px; background: var(--xp-blue); color: #fff; border: none;
  border-radius: 4px; cursor: pointer;
}
.link-embed-duration { color: var(--xp-text-secondary); font-size: 0.8em; margin-top: 4px; }
.link-embed-chip {
  background: var(--xp-panel-bg); border: 1px solid var(--xp-border);
  color: var(--xp-text-normal); border-radius: 4px; padding: 4px 10px;
  cursor: pointer; margin-top: 6px;
}
.link-embed-state { color: var(--xp-text-secondary); font-size: 0.9em; }
```

> For `xp-luna-blue`, use `--xp-text-secondary` where `--xp-text-normal` is undefined (the project notes this theme lacks `--xp-text-normal` in some contexts — check and fall back as the existing invite-embed CSS does). For `hello-kitty`, reuse its accent var (e.g. `--xp-bow-red`) instead of `--xp-blue` if `--xp-blue` is undefined there. Confirm each variable exists in that theme before using it.

- [ ] **Step 2: Verify presence in all themes**

Run: `grep -l "link-embed" client/src/themes/*/theme.css`
Expected: all three theme files listed.

- [ ] **Step 3: Commit**

```bash
git add client/src/themes/discord-dark/theme.css client/src/themes/hello-kitty/theme.css client/src/themes/xp-luna-blue/theme.css
git commit -m "client: embed card CSS in all three themes"
```

---

## PHASE 7 — Data-saver setting

### Task 20: Data-saver toggle

**Files:**
- Modify: the client settings store (find it: `grep -rn "input_device\|output_device" client/src-tauri/src` and the matching frontend settings hook/component, e.g. `client/src/components/VoiceSettings.tsx` or a general settings store).
- Modify: `client/src/components/Message.tsx` (replace the local `const dataSaver = false`).

**Interfaces:**
- Consumes: the existing settings get/set pattern (string/bool keys like `input_device`).
- Produces: a `data_saver_embeds` boolean setting (default false) + a toggle in Settings UI + Message.tsx reading it.

- [ ] **Step 1: Add the setting key (backend)**

Following the exact pattern used for `input_device`/`output_device` (a settings map persisted by the client), add a `data_saver_embeds` boolean key with default `false`. If settings are a typed struct, add the field; if a key-value store, add get/set commands or reuse the generic ones. Mirror the established pattern exactly (do not invent a new settings mechanism).

- [ ] **Step 2: Add the toggle (frontend)**

In the Settings UI where voice device pickers live (or a new "Privacy & Data" section), add a checkbox bound to `data_saver_embeds`:

```tsx
<label className="settings-row">
  <input type="checkbox" checked={dataSaverEmbeds}
         onChange={(e) => setDataSaverEmbeds(e.target.checked)} />
  Data saver: load link previews only when clicked
</label>
```

Wire `dataSaverEmbeds` through the same settings hook other settings use.

- [ ] **Step 3: Consume it in Message.tsx**

Replace the temporary `const dataSaver = false;` with the real value read from settings (via the settings context/hook the app already uses). Pass it to `<LinkEmbed dataSaver={dataSaver} />`.

- [ ] **Step 4: Type-check + build**

Run: `cd client && npx tsc --noEmit` and `cd client/src-tauri && cargo build`
Expected: clean + builds.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "client: data-saver toggle for link embeds (default off)"
```

---

## PHASE 8 — Docs + final gate

### Task 21: Documentation

**Files:**
- Create: `docs/modules/relay-embed.md`
- Modify: `docs/modules/tauri-commands.md`, `docs/modules/tauri-bridge.md`, the protocol module doc (find: `ls docs/modules | grep -i protocol`), `ARCHITECTURE.md`

**Interfaces:** none (docs).

- [ ] **Step 1: Write relay-embed.md**

Document: the allowlist + providers, the `LinkFetcher` seam, `SafeFetcher` guards (allowlist, SSRF on all resolved IPs, redirect re-validation, meta cap, media cap, content-type gate), the cache, and the `ProxyLinkEmbed`/`ProxyMedia` wire exchange (header + length-framed bytes). Note the trust model (relay sees which links are unfurled; viewer IP hidden; DMs/voice still E2EE).

- [ ] **Step 2: Update tauri-commands.md + tauri-bridge.md**

Add `get_link_embed` and `get_proxied_media` entries (params, returns, side effects) naming their `getLinkEmbed`/`getProxiedMedia` bridge fns. Add the two events/commands to tauri-bridge.md.

- [ ] **Step 3: Update protocol doc + ARCHITECTURE.md**

Add the new messages/types to the protocol module doc. In `ARCHITECTURE.md`, note the relay's new fetch-proxy capability (HTTP egress to an allowlist) under the relay section.

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "docs: relay embed proxy module + command/protocol/architecture updates"
```

---

### Task 22: Full regression gate + seam audit

**Files:** none (verification).

- [ ] **Step 1: Workspace tests**

Run: `cd /home/deez/farder && cargo test --workspace`
Expected: PASS (all crates, including the new relay embed tests + integration test).

- [ ] **Step 2: Client crate tests**

Run: `cd /home/deez/farder/client/src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 3: Frontend type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Seam audit**

Run: `grep -nE "get_link_embed|get_proxied_media" /home/deez/farder/client/src-tauri/src/main.rs`
Expected: both names present in `generate_handler!`. Also confirm `getLinkEmbed`/`getProxiedMedia` in `tauri-bridge.ts` use exactly those invoke strings.

- [ ] **Step 5: Final commit (if any doc/cleanup deltas)**

```bash
git add -A && git commit -m "chore: rich external embeds — final gate green" || echo "nothing to commit"
```

---

## Runtime verification (owner, post-merge — UNVERIFIED until done)

Per the verify-before-done rule, the headless gate proves parsing, guards, and transport; it does NOT prove the end-to-end render. After merge:

1. **VPS relay redeploy** (the relay binary now does HTTP fetching):
   `git pull && docker compose -f deploy/relay/docker-compose.yml up -d --build`
2. **Client rebuild** on Windows.
3. In a channel, post: a tweet with video (inline player), a YouTube link
   (thumbnail card → Play opens browser), a direct image (i.redd.it/imgur), a
   Reddit post, a Spotify track. Confirm cards render, the tweet video plays
   inline, IPs never leak (the relay makes the requests).
4. Toggle **data saver** on → embeds become "Load preview" chips.

---

## Self-Review notes (addressed)

- **Spec coverage:** allowlist (Task 3), all 5 providers (Tasks 5,7,8), hybrid video (adapters set `playable_inline`; Task 17 renders inline vs open), media proxy with caps/SSRF/content-type (Tasks 9,10), auto-show + data-saver (Tasks 17,20), client cards in 3 themes (Tasks 17,19), guardrails (Tasks 3,9,10), tests incl. fixtures + integration (Tasks 5–8,12), docs (Task 21), ops note (header + runtime section). All covered.
- **Type consistency:** `LinkEmbed`/`EmbedMedia`/`EmbedKind`/`EmbedOutcome` defined in Task 1 are used verbatim in relay (Tasks 5–11), client commands (Tasks 13–14), and TS mirror (Task 15). `EmbedOutcome` serde shape (`{Embed: x}` vs bare strings) is handled in the TS hook (Task 16).
- **Allowlist parity:** the relay `ALLOWLIST` (Task 3, plus `fxtwitter.com` added in Task 9) and the TS `ALLOWLIST_HOSTS` (Task 15) must stay in sync — note the TS list intentionally omits `fxtwitter.com`/`api.fxtwitter.com` because the client only detects *user-posted* hosts (x.com/twitter.com), not the API host the relay calls internally.
