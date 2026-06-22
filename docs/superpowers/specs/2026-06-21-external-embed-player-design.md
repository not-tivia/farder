# In-App External Embed Player (opt-in iframe) — Design Spec

**Date:** 2026-06-21
**Status:** Approved (brainstorm), pending implementation plan
**Builds on:** the rich external embeds feature (`LinkEmbed`, `useLinkEmbed`,
the relay embed metadata) and the existing settings UI
(`components/settings/SettingsModal.tsx`, `VoiceSettings.tsx`).

## Problem

Today a YouTube or Spotify link in chat renders a thumbnail card with an
"Open externally" button — clicking it launches the system browser / app. The
owner wants the Discord-style experience of **watching the video inside
Farder**, while preserving Farder's privacy promise.

The hard constraint: YouTube's and Spotify's players are live web apps that must
talk to Google/Spotify **from the viewer's machine** to work (DRM, adaptive
streaming, session tokens). Unlike a raw `.mp4` (which the relay already proxies
— see the PiP feature), the relay cannot cheaply proxy these players without
re-implementing an Invidious/yt-dlp-style extractor (heavy, fragile, bandwidth-
and legally-fraught). That relay-proxy path is **explicitly deferred** as a
possible future v2.

So v1 is an **opt-in iframe**: let users watch in-app *if they choose*, behind
an explicit, remembered consent that is honest about the privacy cost.

## Product decisions (owner, locked 2026-06-21)

- **Render inline** in the chat card — the card's thumbnail swaps for the
  provider's embedded player in place; a collapse control removes it. (Not a PiP
  pane in v1; the PiP machinery exists to build on later.)
- **Providers: both YouTube and Spotify.** YouTube plays the full video.
  Spotify in-app plays a **30-second preview** only (the webview is not logged
  into Spotify) — the owner accepts this; the card shows a small "30s preview"
  note. The full-track path remains "Open externally".
- **Consent: privacy-by-default, warn-once-per-provider.** Nothing external
  loads until the user clicks **"Watch here"**. The first click *per provider*
  shows a warning with an **"Always allow <provider>"** checkbox. After consent
  is granted, future cards for that provider load on a single click (no modal).
  Consent is tracked **separately** per provider and is **revocable**.
- **Honesty:** the warning states plainly that watching connects the user to the
  provider and shares their IP and viewing data.

## Privacy posture (what this feature does and does not change)

- **Default behavior is unchanged and leak-free:** on render, a YouTube/Spotify
  card still only shows relay-proxied metadata (title/thumbnail). No connection
  to Google/Spotify happens from rendering a message.
- **The leak is opt-in and consensual:** only after the user clicks "Watch here"
  (and, the first time, accepts the warning) does the iframe load and connect the
  viewer's machine to the provider. This is the inherent, disclosed cost.
- **Hardening (least-leaky version of an inevitably-leaky thing):**
  - YouTube uses the privacy-enhanced **`youtube-nocookie.com`** embed host.
  - The iframe is **sandboxed**:
    `sandbox="allow-scripts allow-same-origin allow-presentation"` (scripts +
    same-origin are required for the players to run; presentation enables
    fullscreen), `referrerpolicy="no-referrer"`,
    `allow="encrypted-media; fullscreen; picture-in-picture"`, `loading="lazy"`.
  - Collapsing the player **unmounts the iframe**, severing the connection.
- This feature is **client-only**: no relay, protocol, or Tauri-command changes.
  Consent is stored in the webview via `localStorage` (a UI preference; keeps the
  feature seam-free per CLAUDE.md's warning about the untyped Tauri seam, and is
  instantly revocable).

## Architecture

Entirely client-side. The relay's existing embed metadata already gives us the
provider and canonical URL; we parse the video/track id from that URL and build
the provider's official embed URL.

```
LinkEmbed (YouTube/Spotify card)
  ├─ thumbnail + "▶ Watch here" + "Open externally"
  └─ click "Watch here"
       ├─ consent[provider] === true  → load <iframe> inline
       └─ consent[provider] !== true  → EmbedConsentModal
                                          └─ Watch (+ optional "always allow")
                                               → (persist if checked) → load <iframe> inline
Settings → (Voice/Privacy section)
  └─ "Allow YouTube embeds" / "Allow Spotify embeds" toggles ↔ same consent store
```

## Components

### A. `lib/embedPlayer.ts` (new)

Pure helpers (test-noted, no React):

- `type EmbedProvider = "youtube" | "spotify"`.
- `buildEmbedPlayerSrc(url: string): { provider: EmbedProvider; src: string } | null`
  — detects the provider from the URL host (`youtube.com`/`youtu.be`/
  `www.youtube.com` → youtube; `open.spotify.com` → spotify; uses `new URL()` in
  a try/catch), parses the id, and returns the provider + embed src, or `null`
  if the host is unsupported or the id is unparseable:
  - YouTube: `watch?v=<id>`, `youtu.be/<id>`, `youtube.com/shorts/<id>`,
    `youtube.com/embed/<id>` → `https://www.youtube-nocookie.com/embed/<id>`.
  - Spotify: `open.spotify.com/<type>/<id>` where type ∈
    {track, album, playlist, episode, show} →
    `https://open.spotify.com/embed/<type>/<id>`.
- Consent store (backed by `localStorage`):
  - keys: `farder.embedConsent.youtube`, `farder.embedConsent.spotify`
    (value `"1"` = allowed; absent/other = not allowed).
  - `getEmbedConsent(p: EmbedProvider): boolean`
  - `setEmbedConsent(p: EmbedProvider, allowed: boolean): void`
  - `providerLabel(p: EmbedProvider): string` → `"YouTube"` / `"Spotify"`.

**Test-notes (verified by inspection, mirroring `detectEmbedUrls`):**
`https://youtu.be/abc123` → `{youtube, .../embed/abc123}`;
`https://www.youtube.com/watch?v=abc123&t=10` → `{youtube, .../embed/abc123}`;
`https://open.spotify.com/track/xyz` → `{spotify, .../embed/track/xyz}`;
`https://example.com/x` → `null`; malformed URL → `null`.

### B. `EmbedConsentModal.tsx` (new)

A small modal reusing the app's existing modal/confirm styling
(e.g. `JoinConfirmModal` patterns and `var(--xp-…)` classes):
- Props: `provider: EmbedProvider`, `onConfirm: (alwaysAllow: boolean) => void`,
  `onCancel: () => void`.
- Copy: **"Watch in Farder?"** / "Playing this connects you directly to
  `<provider>` and shares your IP address and viewing data with them. Farder
  can't hide this while the video plays." + a checkbox **"Always allow
  `<provider>` embeds"** + **Cancel** / **Watch** buttons.
- Returns the checkbox state to `onConfirm`.

### C. `LinkEmbed.tsx` change

In the non-inline provider branch (currently the YouTube/Spotify thumbnail +
`link-embed-play` "Open" button):
- Compute `player = buildEmbedPlayerSrc(e.url)` (only meaningful for
  youtube/spotify; `null` otherwise).
- If `player` is non-null, render, in addition to the existing **"Open
  externally"** button, a **"▶ Watch here"** button and local state
  `watching: boolean`.
- Clicking "Watch here":
  - if `getEmbedConsent(player.provider)` → set `watching = true`.
  - else → open `EmbedConsentModal`; on confirm, if `alwaysAllow`
    `setEmbedConsent(provider, true)`, then set `watching = true`; on cancel,
    do nothing.
- When `watching`, replace the thumbnail with the sandboxed `<iframe>` (props per
  the hardening list above) inside a `.embed-player` container that has a
  **collapse (✕)** button setting `watching = false` (unmounts the iframe).
- Spotify: show a small "30s preview in-app" note next to/under the player.
- If `player` is `null` (id unparseable, or non-youtube/spotify), behavior is
  exactly as today (thumbnail + Open externally only). Inline images and the PiP
  video poster (from the PiP feature) are untouched.
- Rules of hooks: any new hooks (e.g. `useState` for `watching`/modal) are added
  at the top with the other hoisted hooks, before the existing early returns.

### D. Settings revocation UI

In `VoiceSettings.tsx` (where the existing **data-saver embeds** toggle lives,
line ~265 — the natural neighbor, hosted by `components/settings/SettingsModal.tsx`),
add two checkbox toggles:
- **"Allow YouTube embeds (sends your IP to YouTube when you watch)"**
- **"Allow Spotify embeds (sends your IP to Spotify when you watch)"**

Each reads `getEmbedConsent(p)` on mount and writes `setEmbedConsent(p, value)`
on change. Turning a toggle **off** reverts that provider to warn-on-next-watch.
(These are the same `localStorage` flags the modal writes, so the two stay in
sync within a session via a simple re-read; cross-component live sync is not
required — the modal grants, settings revokes, and each reads current state when
shown.)

### E. Theming

New classes styled in **all three** themes (`discord-dark`, `hello-kitty`,
`xp-luna-blue`) using `var(--xp-…)` variables, no hard-coded colors (the one
accepted exception is `#fff`-on-`--xp-blue` matching the existing
`.link-embed-play`):
- `.embed-watch-btn` (the "▶ Watch here" button; mirror `.link-embed-play`),
- `.embed-player` (the inline iframe container — fixed aspect/size, rounded),
- `.embed-player-frame` (the iframe sizing),
- `.embed-player-close` (collapse ✕),
- `.embed-player-note` (the Spotify "30s preview" note),
- `.embed-consent-*` for the modal (or reuse existing modal classes where they
  fit). `xp-luna-blue` lacks `--xp-text-normal`, so text uses
  `var(--xp-text-normal, var(--xp-text-secondary))`.

## Data flow

1. A YouTube/Spotify message renders its card (relay metadata only — no external
   connection).
2. User clicks "▶ Watch here".
3. Consent check: granted → load iframe; not granted → modal → (persist if
   "always allow") → load iframe.
4. The sandboxed `<iframe>` (youtube-nocookie / spotify embed) connects the
   viewer to the provider and plays.
5. Collapse (✕) unmounts the iframe, ending the connection.
6. Settings toggles read/write the same consent flags to revoke.

## Error handling & edge cases

- **Unparseable id / non-supported provider:** no "Watch here" button; the card
  behaves exactly as today (Open externally). `buildEmbedPlayerSrc` returns
  `null` and the UI degrades gracefully.
- **Spotify preview limitation:** surfaced honestly with the "30s preview"
  note so users aren't confused when playback stops.
- **localStorage unavailable/blocked (unlikely in the Tauri webview):**
  `getEmbedConsent` treats any failure as "not allowed" (fail-closed →
  privacy-safe: the worst case is the user is asked again, never that something
  loads without consent). `setEmbedConsent` swallows write errors.
- **Multiple players:** each card manages its own `watching` state; opening one
  doesn't affect others. (No global cap needed — these are inline, not floating.)

## Testing

- **`tsc` clean.**
- **`lib/embedPlayer.ts`** pure helpers verified by inline test-notes
  (URL parsing for both providers + the null cases; consent get/set round-trip).
  No JS test runner exists in the repo (consistent with the rest of the client);
  structure helpers as pure functions a reviewer can verify by inspection.
- **Runtime verification (Windows, per verify-before-done — UNVERIFIED until
  then; WSL has no display):** post a YouTube link → card shows thumbnail +
  "Watch here" → first click shows the warning + "always allow" → Watch → video
  plays inline → collapse removes it → a second YouTube card now plays on one
  click → toggle "Allow YouTube embeds" off in Settings → next card asks again.
  Repeat for a Spotify link (30s preview + note). Confirm a non-parseable link
  still only offers "Open externally".

## Out of scope (explicit)

- **Relay-proxied (Invidious/yt-dlp-style) YouTube playback** — the true-privacy
  path; deferred as a possible v2 with its own brainstorm (extractor choice,
  bandwidth caps, caching, legal stance).
- **Putting the iframe in a PiP pane** — v1 is inline-only; PiP is for
  relay-proxied `<video>`.
- **Generic OpenGraph / arbitrary-site iframes** — only the YouTube/Spotify
  allowlisted providers.
- **Logging into Spotify in-app for full tracks** — out of scope; "Open
  externally" remains the full-track path.

## Documentation (same-commit discipline)

- New `lib/embedPlayer.ts` + `EmbedConsentModal.tsx` + the LinkEmbed/Settings
  changes → note in `docs/modules/relay-embed.md` (where embeds are described)
  that YouTube/Spotify now offer an opt-in in-app iframe player with per-provider
  consent, and document the consent `localStorage` keys.
- No `tauri-commands.md` / protocol / relay doc changes (client-only feature).
