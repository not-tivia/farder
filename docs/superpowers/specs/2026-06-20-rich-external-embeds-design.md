# Rich External Link Embeds — Design Spec

**Date:** 2026-06-20
**Status:** Approved (brainstorm), pending implementation plan
**Builds on:** the invite-preview fetch proxy (`crates/farder-relay/src/proxy.rs`,
`ProxyInvitePreview`), which established the relay-as-privacy-fetch-proxy pattern.

## Problem

When a user posts a link in chat (a tweet, a YouTube video, an image), Farder
shows a bare URL — no preview, no context. Discord-style unfurling is the
product ask (backlog item #2), specifically the fxtwitter/fixupx style of
showing inline social media instead of raw links.

The hard constraint is Farder's core promise: **a user's IP must never leak to a
third-party site.** A naive client-side unfurl (fetch the page / thumbnail
directly) connects the viewer's client straight to Twitter/Google/a CDN and
leaks their IP merely by *seeing* the message. So all fetching must happen on the
relay — extending the "relay is the privacy fetch proxy" architecture already
proven for invite previews.

## Product decisions (owner)

- **Scope:** a **curated allowlist** of supported sites (not generic
  unfurl-anything). Privacy-safest (only known domains are ever fetched), most
  polished cards, and the allowlist is designed to grow. A generic-OpenGraph
  fallback is explicitly a *possible later phase*, not in this spec.
- **v1 sites:** Twitter/X (via fxtwitter), YouTube, direct images, Reddit,
  Spotify.
- **Video behavior — hybrid:**
  - Direct-file video (Twitter/X video, GIFs) → **inline player in the card**,
    bytes proxied through the relay (no IP leak).
  - YouTube → **rich thumbnail card**; Play **opens the user's external
    browser** (the one consensual moment the IP touches Google). No private
    inline YouTube playback (its stream isn't a simple file; the only inline
    option is the YouTube iframe, which leaks IP + loads trackers — rejected).
  - Reddit `v.redd.it` video → card only in v1 (separate DASH audio track makes
    private inline muxing messy; revisit later).
  - Spotify → card; Open opens externally (DRM, no inline playback).
- **Auto-load:** embeds **auto-show** below the message (Discord-style), with a
  **data-saver toggle** built in this feature (owner idea #4 seed). When the
  toggle is on, embeds render as a "Load preview" chip and fetch on click.
  Regardless of the toggle, *video bytes* stream only when the user hits Play.

## Architecture (Approach A — two request types)

Only the relay may touch the internet. Metadata and media bytes flow as two
separate relay requests:

1. `ProxyLinkEmbed { url }` → relay runs the right per-site adapter, normalizes
   to one small `LinkEmbed` struct, returns it (cacheable, ~1h TTL).
2. `ProxyMedia { url }` → relay streams the thumbnail or direct media file
   through itself with strict caps.

The client renders the card from `LinkEmbed`; `<img>`/`<video>` sources point at
a Tauri command that pulls bytes via `ProxyMedia` and wraps them in a blob URL.

Rejected alternatives: (B) one fat request inlining media as base64 — bloats
responses, can't stream video; (C) relay for metadata but client fetches media
direct from the CDN — leaks IP, breaks the core promise.

## Components

### A. Protocol layer (`farder-protocol`)

New relay messages (mirroring `ProxyInvitePreview`/`ProxyInvitePreviewResult`):

- `ProxyLinkEmbed { url: String }`
- `ProxyLinkEmbedResult { outcome: EmbedOutcome }`
- `ProxyMedia { url: String }` — answered with a length-framed byte stream
  prefixed by a small header carrying the validated `content_type` and total
  length (or an error/`Unavailable` marker).

New types:

```text
enum EmbedOutcome { Embed(LinkEmbed), Unsupported, Unavailable }

struct LinkEmbed {
    provider:      String,          // "twitter" | "youtube" | "reddit" | "spotify" | "image"
    kind:          EmbedKind,       // Tweet | Video | Image | Audio | Article
    url:           String,          // canonical link
    title:         Option<String>,
    author:        Option<String>,  // @handle / channel / artist / r/subreddit
    description:   Option<String>,
    thumbnail:     Option<String>,  // URL to fetch via ProxyMedia
    media:         Option<EmbedMedia>,
    duration_secs: Option<u32>,
}

struct EmbedMedia {
    url:             String,        // direct .mp4 / image / gif to fetch via ProxyMedia
    mime:            String,
    width:           Option<u32>,
    height:          Option<u32>,
    playable_inline: bool,          // true for direct-file; false for YouTube/Spotify
}
```

Failure outcomes are uniform and leak nothing about *why* (SSRF refusal,
timeout, non-allowlisted, parse failure all collapse to `Unavailable`;
recognized-but-unhandled URL shapes → `Unsupported`).

Round-trip encode/decode tests mirror the existing `messages.rs` tests.

### B. Relay — embed resolver (`crates/farder-relay`, new `embed.rs`)

- Adds an **HTTP client** (reqwest with rustls) — the one genuinely new
  capability in the relay.
- A **domain allowlist** (compiled-in defaults; optionally extendable via relay
  config) mapping each allowed host to a **per-site adapter**:
  - **Twitter/X** (`twitter.com`, `x.com`, `*.twitter.com`, `*.x.com`) → rewrite
    the status URL to the fxtwitter API, which returns JSON with text, author,
    and direct `.mp4`/image URLs → `playable_inline: true`.
  - **YouTube** (`youtube.com`, `youtu.be`, `*.youtube.com`) → oEmbed
    (`/oembed?url=...&format=json`): title, channel (`author_name`), thumbnail.
    `playable_inline: false`. Duration best-effort (skip if not cheaply
    available).
  - **Reddit** (`reddit.com`, `*.reddit.com`, `redd.it`) → post JSON for
    title/subreddit/thumbnail; `i.redd.it` images render inline; `v.redd.it`
    video is card-only in v1.
  - **Spotify** (`open.spotify.com`) → oEmbed: cover art, title, artist;
    `playable_inline: false`.
  - **Direct images** — to stay consistent with the curated-allowlist
    philosophy, v1 supports a curated set of **common image hosts** (e.g.
    `i.redd.it`, `i.imgur.com`, and similar) rather than any arbitrary host. A
    matched URL whose response is a content-type-verified image within the size
    cap renders inline (bytes proxied). Generic "image on any host" is the same
    *later phase* as generic OpenGraph (see Out of scope).
- Reuses `is_global_ip` (SSRF), a TTL cache (longer TTL for embeds, ~1h), and the
  router per-IP rate limit.

### C. Relay — media proxy (in `embed.rs`)

- `ProxyMedia` streams the thumbnail or direct media file through the relay with
  strict caps:
  - **size cap** enforced during streaming (default ~25 MB; not trusting
    `Content-Length`); over-cap video → the card falls back to open-externally.
  - **content-type allowlist** (`image/*`, `video/mp4`, gif); reject everything
    else.
  - timeout, redirect cap (~3) with SSRF re-validation on every hop.
- The media URL is **re-validated** through the allowlist + SSRF guard before
  fetching (see Security #3).

Both new requests ride the same throwaway pre-auth relay-connection pattern as
`ProxyInvitePreview` (first-message role, no auth, cannot reconnect-loop).

### D. Client — Tauri commands (`commands.rs`) + bridge (`tauri-bridge.ts`)

- `get_link_embed(url) -> EmbedOutcome` — opens a connection to the **default
  relay** (`default_relay()`; embeds aren't tied to the current server, so this
  works on relayed *and* direct servers), sends `ProxyLinkEmbed`, returns the
  outcome. Short client-side cache (like `get_invite_preview`).
- `get_proxied_media(url) -> bytes` — pulls bytes via `ProxyMedia` and returns
  them to JS, which builds a **blob URL** (`URL.createObjectURL`) for
  `<img>`/`<video>` (supports video seeking; revoked to free memory). The webview
  never fetches the CDN directly.
- Both registered in `generate_handler!` and exposed as `getLinkEmbed` /
  `getProxiedMedia`. The `invoke` names, the `#[tauri::command] fn`s, and the
  handler list must agree (zero seam drift — the project's known failure mode).

### E. Client — link detection + rendering

- A client-side regex scans message text for URLs whose host is on the allowlist
  (mirrors how `InviteEmbed` detects invite links). Only matched domains trigger
  a fetch; cap ~3 embeds/message; skip deleted messages; dedupe by canonical URL.
- New **`LinkEmbed.tsx`** rendered below the message body alongside the existing
  `InviteEmbed` (Message.tsx already has that embed/attachment region). Card
  variants by provider/kind:
  - **Tweet** → author + text + inline image, or inline `<video controls>` (blob
    URL) for video.
  - **YouTube** → proxied thumbnail + title + channel + duration; **Play opens
    the external browser** (the existing external-open path used by invite
    links/pills).
  - **Image** → inline `<img>` (proxied), click to enlarge.
  - **Reddit / Spotify** → cover/thumbnail + title + author; Open opens
    externally.
- **Lazy video bytes:** the card + thumbnail load on display; the video file
  streams only when the user clicks Play.

### F. Client — data-saver setting

- A settings toggle using the existing settings-store pattern (like
  `input_device`/`output_device`). Default **off** (auto-show). When **on**,
  embeds render as a compact "Load preview" chip that fetches on click. Lives in
  a "Privacy & Data" area of Settings.

### G. Theming

- All new card classes added to **all three** theme files (`discord-dark`,
  `hello-kitty`, `xp-luna-blue`) driven by CSS variables — never hard-coded
  colors (per the project styling rule). Reuse existing card/border/text vars.

## Security & guardrails

1. **Allowlist with safe host matching** — only allowlisted hosts are fetched.
   Match on the parsed registrable domain (and explicit subdomains); reject
   lookalikes (`youtube.com.evil.com`) and raw-IP hosts.
2. **SSRF defense-in-depth** — even allowlisted domains: resolve the hostname and
   check *every* resolved IP with `is_global_ip`; refuse if any is
   private/loopback (DNS-rebinding). Re-validate on every redirect hop; redirect
   cap ~3.
3. **Media-URL re-validation (key)** — the direct media/thumbnail URL extracted
   by an adapter is re-validated through the allowlist + SSRF guard before
   `ProxyMedia` fetches it, so a malicious page can't make the relay fetch an
   internal URL by labeling it "thumbnail."
4. **Size caps enforced while streaming** — count bytes, abort past the cap; do
   not trust `Content-Length`. ~16 KB metadata, ~25 MB media.
5. **Content-type allowlist for media** — `image/*`, `video/mp4`, gif only;
   reject HTML/scripts/octet-stream.
6. **Timeouts** (~5–8 s metadata; bounded media budget), **per-IP rate limit**
   (reuse router limit; a separate bucket for bandwidth-heavy media), **TTL
   cache** (~1 h metadata) — the main bandwidth/abuse defense on the $6 VPS.
7. **No code execution** — relay parses OG tags / oEmbed JSON only; never runs
   JS; never auto-follows sub-resources beyond the single media URL the client
   explicitly requests.
8. **Uniform failure + throwaway pre-auth connection** — outcomes leak nothing;
   cannot reconnect-loop (same posture as invite previews).
9. **Trust model, documented honestly** — the viewer's IP never reaches the site
   (the relay does); the relay operator *can* see which links a user unfurls
   (same Signal-like trust as the rest of the relay; DMs/voice stay E2EE). Folds
   into the existing relay-content disclosure.

## Testing

- **Relay unit tests, fully headless (no live network):**
  - Adapters parse **canned fixtures** (saved fxtwitter JSON, YouTube oEmbed,
    Spotify oEmbed, OG HTML, Reddit JSON) → assert normalized `LinkEmbed` fields.
  - Allowlist accept/reject, including lookalikes and raw-IP hosts.
  - SSRF refusal, including a resolver that returns a private IP for an
    allowlisted name → refused.
  - Streaming size-cap abort past a lying `Content-Length`.
  - Content-type rejection of non-media responses.
  - Cache TTL/pressure (reuse the `PreviewCache` test pattern).
  - Media-URL re-validation: an adapter-returned media URL that is
    off-allowlist/non-global is refused.
  - Adapters take an **injectable HTTP/validator seam** so an integration test
    can point at a localhost fixture HTTP server with the SSRF guard relaxed
    *in test only* — the production guard stays correct.
- **Protocol round-trip tests** for the new messages + `LinkEmbed`/`EmbedOutcome`.
- **Client:** tsc clean; link-detection regex unit tests (allowlist match,
  lookalike/deleted skip, dedupe, cap-3); the mechanical **seam audit** (every
  `invoke` name appears in `generate_handler!`).
- **Runtime gate (verify-before-done):** headless proves parsing, guards, and
  transport; the real card-renders-and-video-plays test needs the GUI + a relay
  redeploy + client rebuild, so it ships **UNVERIFIED until an owner Windows run
  against the redeployed VPS relay**, called out explicitly.

## Ops

- **Requires a VPS relay redeploy** (the relay binary gains HTTP fetching):
  `git pull && docker compose -f deploy/relay/docker-compose.yml up -d --build`.
- **Requires a client rebuild** (new commands + UI; client reads
  `default_relay()` for embed fetches).

## Documentation (same-commit discipline)

- New relay module doc `docs/modules/relay-embed.md` (or extend the relay/proxy
  module doc) for the embed resolver + media proxy.
- New Tauri commands → `docs/modules/tauri-commands.md` + named in
  `tauri-bridge.md`.
- New protocol messages/types → the protocol module doc.
- New React component / settings key → frontend docs.
- `ARCHITECTURE.md` updated to note the relay's new fetch-proxy capability.

## Out of scope (explicit)

- Generic OpenGraph unfurl for arbitrary domains, and inline images from
  arbitrary (non-allowlisted) image hosts (both a possible later phase).
- Private inline YouTube playback / proxied adaptive streams.
- Reddit `v.redd.it` inline video (audio-track muxing).
- A full data-saving mode beyond the embed toggle (owner idea #4 in full).
- Per-server / per-channel embed enable-disable controls.
