# Relay embed proxy

> **File(s):** `crates/farder-relay/src/embed.rs`, `crates/farder-relay/src/router.rs` (handlers `handle_link_embed` + `handle_media`)
> **Layer:** Server crate (relay)
> **Last reviewed:** 2026-06-21

## Purpose

This module is the relay's rich-embed fetch proxy — phase two of the relay's
fetch-proxy capability (phase one is invite previews). It lets the client resolve
external-URL metadata (title, author, thumbnail, media) and stream media bytes
(images, direct video) through the relay, so the viewer's IP is never exposed to
any third-party CDN or social platform. The relay also caches resolved metadata
to reduce egress and rate-limit thrash.

The module owns: the host allowlist, URL classification, the `LinkFetcher` seam,
the `SafeFetcher` production implementation (with all egress guardrails), the
per-provider adapters, the `EmbedCache`, and the relay-side request handlers.

---

## Allowlist and providers

The relay maintains a hard-coded static allowlist of registrable-domain suffixes.
A URL host matches an entry if it equals the suffix exactly **or** ends with
`"." + suffix` (subdomain match). Bare IP hosts are rejected outright, before the
allowlist check, so `http://1.2.3.4/x` can never match even if an attacker crafts
a suffixed hostname.

Longest-suffix wins, so `i.redd.it` (Image provider) beats `redd.it` (Reddit
provider) for hosts under `i.redd.it`.

| Allowlist host | Provider |
|---|---|
| `twitter.com` | Twitter |
| `x.com` | Twitter |
| `fxtwitter.com` | Twitter (internal API relay rewrite target) |
| `youtube.com` | YouTube |
| `youtu.be` | YouTube |
| `reddit.com` | Reddit |
| `redd.it` | Reddit |
| `open.spotify.com` | Spotify |
| `i.redd.it` | Image |
| `i.imgur.com` | Image |

`fxtwitter.com` is listed because the Twitter adapter rewrites `x.com` / `twitter.com`
status URLs to `api.fxtwitter.com` — a user never posts an `fxtwitter.com` URL;
the relay hits it internally.

---

## LinkFetcher seam

```rust
pub trait LinkFetcher: Send + Sync {
    async fn fetch_text(&self, url: &str) -> Result<FetchedText>;
}

pub struct FetchedText {
    pub body: String,
    pub final_url: String,
}
```

`LinkFetcher` is the seam that makes all adapters unit-testable without a network.
`resolve_embed` and all per-adapter functions are generic over `F: LinkFetcher + ?Sized`
(not `&dyn`, not boxed — the trait bound allows both static dispatch in production
and mock structs in tests). The production implementation is `SafeFetcher`; tests
use `MockFetcher` (a `HashMap<url, body>` map) or `LocalFetcher` (routes to a
localhost fixture server).

---

## SafeFetcher guards

`SafeFetcher` is the production `LinkFetcher`. It enforces all egress guardrails:

1. **Allowlist gate** — `host_is_allowlisted(url)` must return true. Rejects
   non-allowlisted hosts, bare IPs, non-HTTP(S) schemes, and malformed URLs before
   any network activity.

2. **SSRF guard on all resolved IPs** — for every URL (including on each redirect
   hop), all IP addresses the host resolves to must pass `is_global_ip()`. Any
   private, loopback, link-local, or multicast address causes an immediate refusal.
   This prevents DNS rebinding: the guard checks every resolved address, not just
   one.

3. **Manual redirect loop with re-validation on every hop** — `SafeFetcher` sets
   `redirect::Policy::none()` on the underlying `reqwest::Client` and handles
   redirects itself, up to **4 hops**. Each `Location` header is resolved as a
   relative redirect against the current URL, then validated again by
   `validate_fetchable` (allowlist + SSRF) before the next request. This prevents
   "allowlisted-to-private-via-redirect" attacks (the "media-URL trap").

4. **Metadata body cap (META_CAP = 16 KB)** — after a successful 2xx response, the
   body is read fully and rejected if it exceeds 16 384 bytes. Prevents OOM from
   huge HTML pages or JSON payloads.

5. **Timeout (8 s)** — the underlying `reqwest::Client` has a `.timeout(8s)`
   configured. `handle_link_embed` adds a separate `tokio::time::timeout(10s)` on
   the whole `resolve_embed` call (the relay gives the resolver a 10 s outer budget,
   which is slightly looser than the per-request 8 s, covering adapter logic time).

6. **User-Agent** — identifies outbound requests as `FarderRelay/1.0 (+https://farder.gg)`.

`fetch_media` (used by `handle_media`, not via `LinkFetcher`) applies the same
allowlist + SSRF gate, then enforces:

- **Content-type gate** — only `image/*` and `video/mp4` are allowed. Parameters
  are stripped before comparison (e.g. `image/jpeg; charset=binary` becomes
  `image/jpeg`). Anything else (HTML, JSON, binary blobs) is rejected.

- **Streaming byte cap (MEDIA_CAP = 25 MB)** — bytes are accumulated chunk by
  chunk; if `accumulated + chunk > 25 * 1024 * 1024`, the fetch is aborted. The
  cap is enforced on actual bytes read, not on the `Content-Length` header (which
  is untrusted).

---

## EmbedCache

`EmbedCache` is an in-process, `Mutex<HashMap<String, (Instant, EmbedOutcome)>>`
keyed by the raw URL string.

- **TTL: 1 hour** (3 600 s). A cache hit is returned without any network I/O.
- **Max size: 2 048 entries**. On overflow, TTL-expired entries are pruned first;
  if still over capacity, the map is cleared entirely (simple eviction). The map
  is never unbounded.
- **Negative caching:** `Unsupported` and `Unavailable` outcomes are cached
  identically to successful `Embed` outcomes. This prevents hammering the allowlist
  or third-party APIs on repeated requests for the same bad URL.

Cache key is the raw URL string (`embed_cache_key`). The client also maintains its
own 5-minute session cache (in `commands.rs` `LINK_EMBED_CACHE`) so the relay is
rarely hit more than once per unique URL per session.

---

## Provider adapters

Each adapter translates an allowlisted URL into a `LinkEmbed` struct or returns
`None` (→ `EmbedOutcome::Unsupported`). All adapters are generic over
`F: LinkFetcher + ?Sized`.

| Provider | Adapter | API used | Notes |
|---|---|---|---|
| Twitter | `adapt_twitter` | [fxtwitter](https://api.fxtwitter.com) JSON API | URL rewritten via `fxtwitter_api_url` (`/<user>/status/<id>` shape required; non-status URLs → `None`). Prefers video over photo. `playable_inline: true` for both. |
| YouTube | `adapt_youtube` | YouTube oEmbed (`youtube.com/oembed?format=json&url=…`) | Returns `EmbedKind::Video`; `media` is always `None` (YouTube links open externally, not inline). |
| Spotify | `adapt_spotify` | Spotify oEmbed (`open.spotify.com/oembed?url=…`) | Returns `EmbedKind::Audio`; `media` is always `None`. |
| Reddit | `adapt_reddit` | Reddit JSON API (appends `/.json` to the post URL, strips query/fragment) | Returns `EmbedKind::Article`. Thumbnail is only set if it's an `http`/`https` URL (Reddit sentinel values like `"self"` and `"nsfw"` are skipped). |
| Image | `adapt_image` | None (no outbound fetch) | Returns `EmbedKind::Image` with `playable_inline: true`. The image bytes are fetched later via `ProxyMedia`. |

---

## ProxyLinkEmbed wire exchange

`handle_link_embed` handles `Message::ProxyLinkEmbed { url }` on a fresh
throwaway QUIC connection (first message on that connection).

Flow:
1. **URL length check** — URLs longer than 2 048 bytes are rejected immediately
   with `EmbedOutcome::Unavailable`.
2. **Rate-limit** — 30 metadata requests / min / IP bucket (`embed.limiter`).
   Over-limit → `Unavailable`.
3. **Cache check** — if the URL is in `EmbedCache` (within 1 h TTL), the cached
   outcome is returned immediately.
4. **Resolve** — `resolve_embed(url, fetcher)` is called with a 10 s outer
   `tokio::time::timeout`. Timeout → `Unavailable`.
5. **Cache store** — the outcome is cached regardless of variant.
6. **Reply** — `Message::ProxyLinkEmbedResult { outcome }` is framed and written.

Wire format (same 4-byte-BE-length framing as the rest of the relay protocol):
```
[4-byte BE length][MessagePack-encoded ProxyLinkEmbed]   <- client sends
[4-byte BE length][MessagePack-encoded ProxyLinkEmbedResult]  <- relay replies
```

---

## ProxyMedia wire exchange

`handle_media` handles `Message::ProxyMedia { url }` on a separate fresh
throwaway QUIC connection.

Flow:
1. **URL length check** — same 2 048-byte cap.
2. **Rate-limit** — separate, tighter bucket: 60 media requests / min / IP
   (`embed.media_limiter`). Over-limit or bad URL → `ProxyMediaUnavailable`.
3. **Fetch** — `fetch_media(fetcher.client(), url)` is called with a 20 s outer
   `tokio::time::timeout`. All `SafeFetcher` guardrails apply (allowlist, SSRF,
   content-type gate, 25 MB byte cap).
4. **Reply on success:** `ProxyMediaHeader` (framed) then raw bytes length-framed
   (4-byte BE length + raw bytes, NOT a framed protocol message):
   ```
   [4-byte BE framed ProxyMediaHeader { content_type, total_len }]
   [4-byte BE u32 = byte count][raw media bytes]
   ```
5. **Reply on failure:** `ProxyMediaUnavailable` (framed), then close.

Note: the raw bytes segment uses a separate 4-byte BE length-prefix that is NOT
the relay's normal `write_message` framing — it is written directly as
`send.write_all(&(bytes.len() as u32).to_be_bytes())` + `send.write_all(&bytes)`.
The client side (`commands.rs::get_proxied_media`) reads `total_len` from the
header and then `recv.read_exact` for 4 bytes + exact byte count; it verifies
that the framed u32 matches `total_len`.

---

## Trust model

- **Relay sees which links are unfurled.** The relay knows which URL each client
  sends via `ProxyLinkEmbed` and `ProxyMedia` (on separate throwaway connections).
  This is a deliberate privacy trade-off: the relay learns less than the CDN would
  (no cookies, no persistent sessions), but the relay operator can log URLs.
  Self-hosted relay deployment eliminates this.

- **Viewer IP is hidden from third parties.** No third-party CDN, social platform,
  or image host ever sees the viewer's IP. All outbound HTTP(S) traffic originates
  from the relay.

- **DMs and voice remain E2EE end-to-end.** The embed proxy path is completely
  separate from the main relay routing logic. The relay that carries encrypted DMs
  and voice datagrams never inspects those payloads; the embed proxy path does its
  own outbound fetching only when asked by a client on a throwaway connection. The
  E2EE guarantee for DMs and voice is not weakened by this feature.

- **No PII in cache.** The `EmbedCache` is in-process and not persisted to disk.
  It lives only for the relay process lifetime.

---

## State it owns

| Field | Type | What it tracks |
|---|---|---|
| `EmbedContext::cache` | `EmbedCache` (in `Arc<EmbedContext>`) | Resolved embed outcomes, 1 h TTL, 2048-entry cap |
| `EmbedContext::limiter` | `ConnectionLimiter` | 30 metadata req/min/IP rate bucket |
| `EmbedContext::media_limiter` | `ConnectionLimiter` | 60 media req/min/IP rate bucket |
| `EmbedContext::fetcher` | `Arc<SafeFetcher>` | Shared reqwest client with 8 s timeout |

---

## Frontend surfaces

The following client-side files consume the relay embed protocol.

### `client/src/lib/linkEmbed.ts` — types + URL detection

Defines the TypeScript mirror types (`EmbedKind`, `EmbedMedia`, `LinkEmbed`,
`EmbedOutcome`) and the `detectEmbedUrls(text: string): string[]` utility.

`detectEmbedUrls` scans message text for `https?://...` URL patterns, filters
to hosts that are user-postable and allowlisted (excludes internal relay API
hosts like `api.fxtwitter.com`), deduplicates, and caps at **3** URLs per
message. The client-side host list must be kept in sync with the relay's
`ALLOWLIST` in `embed.rs`.

### `client/src/hooks/useLinkEmbed.ts` — embed metadata hook

```ts
function useLinkEmbed(url: string, enabled: boolean): State
```

React hook. When `enabled` is `true`, calls `getLinkEmbed(url)` (which goes to
the relay via `get_link_embed`) and returns:
- `{ status: "loading", embed: null }` — in flight.
- `{ status: "ok", embed: LinkEmbed }` — resolved.
- `{ status: "unsupported" | "unavailable", embed: null }` — no embed available.

Cancels stale fetches on URL/enabled change via an `alive` flag. Errors from
the Tauri command resolve to `"unavailable"`.

The `enabled` flag is the data-saver gate: the `LinkEmbed` component passes
`false` until the user clicks "Load preview", so no relay connection is opened
in data-saver mode.

### `client/src/hooks/useProxiedMedia.ts` — media blob URL hook

```ts
function useProxiedMedia(url: string | null, enabled: boolean): string | null
```

React hook. When `url` is non-null and `enabled` is `true`, calls
`getProxiedMedia(url)`, decodes the `data_base64` field into a `Uint8Array`,
wraps it in a `Blob` with the validated `content_type`, and returns a
`URL.createObjectURL()` blob URL. Returns `null` while loading or on error.
Revokes the blob URL on cleanup (unmount / url or enabled change) to prevent
memory leaks.

### `client/src/components/LinkEmbed.tsx` — embed card component

```tsx
function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean })
```

Renders a rich embed card. Uses `useLinkEmbed` for metadata and
`useProxiedMedia` for inline media (video / image) and thumbnails. All four
`useProxiedMedia` calls are hoisted before any early returns to obey the Rules
of Hooks.

Data-saver behaviour: if `dataSaver` is `true`, the component renders a "Load
preview" `<button>` chip and does not call any relay commands until clicked.

Inline-playable media (`EmbedMedia.playable_inline === true`) renders as an
`<img>` (for `image/*` MIME) or, for `video/*`, as a compact poster (thumbnail
+ ▶ Play button). Non-inline media (YouTube, Spotify) renders a thumbnail with
an external-open button.

**Update (2026-06-21) — opt-in in-app player:** YouTube and Spotify cards now
offer a **"Watch here"** button alongside "Open externally". Clicking it loads a
**sandboxed iframe** inline (YouTube via `youtube-nocookie.com`; Spotify via
`open.spotify.com/embed`, which is a 30s preview in-app). Privacy-by-default:
nothing external loads until the click, and the FIRST click per provider shows a
warning (`EmbedConsentModal`) with an "always allow" checkbox. Consent is stored
client-side in `localStorage` keys `farder.embedConsent.youtube` /
`farder.embedConsent.spotify` (no Tauri/relay involvement) and is revocable in
Settings → Privacy & Data. URL→embed-src parsing and the consent store live in
`client/src/lib/embedPlayer.ts`. The relay-proxied (Invidious-style) path that
would hide the viewer's IP from the provider is explicitly deferred (possible v2).

**Update (2026-06-21):** Playable *video* embeds no longer auto-play inline.
`LinkEmbed` now renders a compact poster (thumbnail + ▶ Play); clicking Play
opens the video in a floating in-app picture-in-picture pane (see
`docs/modules/frontend-pip.md`). The video bytes are fetched only when the PiP
opens (not on card display). Inline images are unchanged. (YouTube/Spotify cards
later gained an opt-in "Watch here" in-app player — see the 2026-06-21 opt-in
in-app player note above.)

Returns `null` for `Unsupported` and `Unavailable` outcomes (renders nothing).

---

## Integration map

- **`router.rs`** — `handle_connection` dispatches `Message::ProxyLinkEmbed` to
  `handle_link_embed` and `Message::ProxyMedia` to `handle_media`, passing the
  shared `Arc<EmbedContext>`.
- **`farder-protocol::messages`** — defines `EmbedKind`, `EmbedMedia`, `LinkEmbed`,
  `EmbedOutcome`, `ProxyLinkEmbed`, `ProxyLinkEmbedResult`, `ProxyMedia`,
  `ProxyMediaHeader`, `ProxyMediaUnavailable`.
- **`crate::proxy::is_global_ip`** — SSRF guard utility shared with the invite-
  preview proxy. Called by `validate_fetchable` inside both `SafeFetcher` and
  `fetch_media`.
- **`client/src-tauri/src/commands.rs`** — `get_link_embed` and `get_proxied_media`
  open throwaway QUIC connections to the relay and speak this protocol.
- **`docs/modules/relay-proxy.md`** — covers phase-one (invite preview) of the
  same relay fetch-proxy capability.

## Known gotchas

- **`fxtwitter.com` in the allowlist** is not a user-postable host; it is
  allowlisted only so the relay can call `api.fxtwitter.com` in the Twitter
  adapter. The `detectEmbedUrls` client-side filter excludes it from the user-
  facing list to prevent confusion.
- **Media bytes NOT a framed protocol message** — the raw bytes after
  `ProxyMediaHeader` are written as a bare 4-byte-BE-length + bytes, bypassing
  `write_message`. The client must not try to decode them as a `Message`.
- **`MEDIA_CAP` is enforced on accumulated bytes, not `Content-Length`** — an
  attacker cannot bypass the cap by lying about content length.
- **Redirect re-validation** — every hop in a redirect chain is individually
  SSRF-checked and allowlist-checked. An allowlisted host that redirects to a
  private address will be refused at the second hop.
- **Metadata timeout is 10 s (outer) + 8 s (per-request)** — the plan mentioned
  8 s for metadata; the code uses 8 s for the per-HTTP-request `reqwest::Client`
  timeout and a 10 s `tokio::time::timeout` in `handle_link_embed` wrapping the
  full `resolve_embed` call (adapter logic + HTTP).
