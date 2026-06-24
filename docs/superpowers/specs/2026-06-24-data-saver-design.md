# Data Saver Mode — Design Spec

**Date:** 2026-06-24
**Status:** Approved (brainstorm complete, ready for implementation plan)
**Scope:** Client-only (React/TS). No Rust, protocol, relay, or Tauri-command changes. Hot-reloads; no sidecar rebuild.

## Goal

Give the user a "Data Saver" mode that reduces bandwidth (and incidental CPU)
by not auto-downloading large media, with a master switch plus per-type
sub-toggles and a configurable size threshold.

## Background / current state

- A single Rust-persisted setting `data_saver_embeds` already exists
  (`commands.rs` `get/set_data_saver_embeds`, `read_data_saver_embeds`) and
  drives the "Load preview" chip in `LinkEmbed.tsx`. **Each `Message` reads it
  via its own IPC call** (`Message.tsx` `getDataSaverEmbeds()` in a
  `useEffect`) — a known latent per-message-IPC perf issue.
- Images render in `AttachmentDisplay` (inside `Message.tsx`), which
  auto-downloads `image/*` and `audio/*` attachments on mount via
  `api.downloadFile(serverId, file_id)` and caches data URLs in a
  module-level `imageCache`. `AttachmentInfo` carries `size`, `width`,
  `height`, `mime_type` — so size is known **without** downloading.
- Embed videos (tweet/direct-video links) render a ▶ poster in `MediaSlot`;
  the bytes stream only when the player opens (`useProxiedMedia` inside
  `MediaPlayer`). Video file attachments are not auto-downloaded
  (`AttachmentDisplay` early-returns for non-image/non-audio). **Therefore
  video bytes are already never auto-fetched — no video gating is needed.**
- Avatars render through one shared `MemberAvatar` component
  (`<img class="avatar-img" src={dataUrl}>`), used by the member list, chat,
  and the profile popup. Avatar bytes are already downloaded and hash-cached
  by profile-sync.

## Global Constraints

- **Client-only.** No changes to Rust, protocol, relay, or Tauri commands.
- **Single source of truth:** all Data Saver state lives in one
  localStorage-backed store (`farder.dataSaver`), read through a React context.
  No per-message IPC.
- **Theme compatibility (REQUIRED):** any new `className` must have CSS in all
  three theme files (`client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css`)
  driven by `var(--xp-*)` variables. No hard-coded colors (except the existing
  `#fff`-on-accent / rgba-shadow conventions). `xp-luna-blue` lacks
  `--xp-text-normal` → use `var(--xp-text-normal, var(--xp-text-secondary))`.
- **Type-check must pass:** `cd client && npx tsc --noEmit` clean. Farder has no
  JS test runner; store logic is written as small pure functions verified by
  type-check + runtime.
- **Default OFF** (opt-in). When the master is on, sub-toggles default on and
  threshold defaults to 1 MB.

## Settings model

New module `client/src/lib/dataSaver.ts`:

```ts
export interface DataSaverSettings {
  enabled: boolean;            // master switch
  gateImages: boolean;         // images over threshold -> click-to-load
  clickToLoadEmbeds: boolean;  // link previews -> "Load preview"
  freezeAvatars: boolean;      // animated avatars -> still first frame
  thresholdMB: number;         // size cutoff for images
}

export const DATA_SAVER_DEFAULTS: DataSaverSettings = {
  enabled: false,
  gateImages: true,
  clickToLoadEmbeds: true,
  freezeAvatars: true,
  thresholdMB: 1,
};
```

- Stored as one JSON object under localStorage key `farder.dataSaver`.
- `getDataSaver(): DataSaverSettings` — reads + parses + fills missing keys
  from defaults; fail-safe to defaults on parse error.
- `setDataSaver(next: DataSaverSettings): void` — writes JSON.
- `thresholdBytes(s)` helper = `s.thresholdMB * 1024 * 1024`.
- `imageIsGated(s, sizeBytes)` helper =
  `s.enabled && s.gateImages && sizeBytes > thresholdBytes(s)`.

**Context:** `client/src/context/DataSaverContext.tsx` exposing
`DataSaverProvider` (holds the settings in state, persists on change) and
`useDataSaver(): { settings, update(patch: Partial<DataSaverSettings>): void }`.
Mounted high in the tree (near the other providers in `AppShell`/`App`) so
every component reads synchronously.

**One-time migration:** on first `DataSaverProvider` mount, if
`localStorage["farder.dataSaver"]` is absent, call the existing
`getDataSaverEmbeds()` once and seed `{ enabled: <that>, clickToLoadEmbeds:
<that> }` over the defaults, then persist. After that the localStorage store is
authoritative and the Rust `data_saver_embeds` path is no longer read by the UI
(commands left in place, dormant — not removed, to avoid a Rust change).

## Settings UI (`VoiceSettings.tsx`, "Privacy & Data" section)

Replace the single embeds checkbox with a master + sub-toggle block:

```
[x] Data Saver
      [x] Don't auto-load large images
      [x] Click-to-load link previews
      [x] Freeze animated avatars
      Auto-load media up to  [ 1 ] MB
```

- Master checkbox bound to `settings.enabled`.
- Sub-rows bound to `gateImages` / `clickToLoadEmbeds` / `freezeAvatars`;
  `disabled` (and visually greyed) when `!enabled`.
- Threshold is a small numeric `<input type="number" min=0 step=0.5>` (MB),
  bound to `thresholdMB`, also disabled when `!enabled` or `!gateImages`.
- Uses existing settings/section classes; new classes (e.g. a `.ds-suboption`
  indent) added to all three themes.

## Image gating (`AttachmentDisplay` in `Message.tsx`)

- Read `useDataSaver()`. Compute `gated = imageIsGated(settings,
  attachment.size)` **and** not already in `imageCache` **and** not
  user-loaded.
- The existing auto-download `useEffect` must **not** fire when `gated` (guard
  its body). Already-cached images always show (never re-hidden).
- When `gated`, render a placeholder button instead of the image:
  - Label: `⬇ Load image (<human size>)` where human size formats
    `attachment.size` (KB/MB, one decimal).
  - The placeholder reserves layout using `attachment.width`/`height`
    (aspect-ratio box, capped to the same max display dimensions the loaded
    image uses) so loading does not shift layout.
  - Click sets a local `userLoaded` state → the existing download path runs →
    image renders exactly as today.
- Below threshold, any toggle off, or cache hit → unchanged (auto-loads).
- Audio/voice notes unaffected. Message GIFs are images → follow the same size
  gate. Each image in a multi-image message gates independently.
- New class `.attachment-load-gate` (button + sized placeholder) styled in all
  three themes.

## Embed gating (`LinkEmbed.tsx`, `Message.tsx`)

- `LinkEmbed` already renders a "Load preview" chip from a `dataSaver` prop.
  Change it to read `useDataSaver().settings.clickToLoadEmbeds` **directly** and
  **drop the `dataSaver` prop**.
- Remove the per-`Message` `getDataSaverEmbeds()` `useEffect` + `dataSaver`
  state and the prop passed at the `<LinkEmbed>` call site. No behavior change
  for the user; fixes the per-message IPC.

## Avatar freezing (`MemberAvatar.tsx`)

- Read `useDataSaver()`. Compute `freeze = settings.enabled &&
  settings.freezeAvatars && isAnimatedMime(dataUrl)`, where `isAnimatedMime`
  inspects the data-URL prefix for `image/gif`, `image/apng`, `image/webp`.
- When `freeze`: render the image into a `<canvas>` sized to the avatar
  display box; on the underlying image's `load`, `drawImage` once (captures the
  first frame) and show the canvas in place of the `<img>`.
- Otherwise render the existing `<img class="avatar-img">` unchanged. Static
  images and the letter-initial fallback are untouched.
- Pure render-time: no network/bandwidth (avatars already cached). One
  component change covers member list, chat, and popup.
- The canvas element reuses `.avatar-img` sizing (or a sibling
  `.avatar-img` + `display:block`); confirm it looks identical across themes.

## Error handling / edge cases

- localStorage parse failure → fall back to `DATA_SAVER_DEFAULTS` (never throw).
- Missing `attachment.width`/`height` → placeholder falls back to a default
  reserved box (same default the loaded image would use); no crash, minor
  layout shift acceptable.
- Canvas `drawImage` failure (e.g. tainted/oversized) → fall back to the plain
  `<img>` (animated, but visible) rather than a blank avatar.
- Migration when `getDataSaverEmbeds()` rejects → just use defaults.

## Testing / verification

- `cd client && npx tsc --noEmit` clean.
- Client-only → verify with `git pull` + `Ctrl+Shift+R` (no sidecar rebuild):
  1. Master off → everything auto-loads as today.
  2. Master on: big image → `⬇ Load image (X MB)` at correct size → click
     loads in place; small image auto-loads.
  3. Change threshold → a previously-gated image now auto-loads (and vice
     versa).
  4. Link previews show "Load preview"; click loads.
  5. Animated-GIF avatar stops animating (still visible); toggling off
     re-animates it.
  6. Settings persist across restart; prior embeds setting carried over via
     migration.
- Theming: new classes present in all three theme files
  (`grep -l "<class>" client/src/themes/*/theme.css` lists all three).

## Out of scope (YAGNI)

- Video gating (videos are already click-to-play; bytes never auto-fetched).
- A quick/global toggle outside Settings.
- Outgoing/upload-side warnings.
- Removing the dormant Rust `data_saver_embeds` command (left in place to keep
  this client-only).
- Relay-proxied YouTube, data-cap accounting/metering.
