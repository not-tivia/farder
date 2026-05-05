# GIF Search (Tenor) — Design

**Status:** Approved 2026-05-05
**Scope:** Farder client (Tauri + React). No server changes (no protocol additions). Builds on Reaction Book Phase 2 (uses the existing `fetch_url` + `book_save_from_url` flow).

## Goal

Let users search and send GIFs from Tenor (Google's open GIF API) directly from the message input. Opt-in for privacy. Saves can flow into the user's Reaction Book like any other image. Matches the Discord experience while honoring Farder's privacy ethos through an explicit opt-in step.

## Non-Goals

- **Giphy support.** Tenor only — Tenor's free tier is more generous, content moderation is built in, and supporting both means double the maintenance.
- **Tenor URL-as-attachment storage.** GIFs sent through the picker are downloaded to the current server's attachment store via the existing `fetch_url` flow. Recipients see a normal server-hosted attachment, not a Tenor URL. This avoids leaking Tenor refs to other clients and means receivers don't need GIF search enabled to view sent GIFs.
- **Trending categories / collections / reactions UI.** Just search + trending list, no curation features.
- **Custom Tenor sticker pack subscriptions.** Out of scope.
- **Audio (Tenor doesn't have it for GIFs anyway, just noting).**

## Architecture

### Tauri-side proxy

A new `client/src-tauri/src/tenor.rs` module owns all Tenor API calls. The renderer process never sees the API key. Two query commands + two settings commands:

```rust
#[tauri::command] pub async fn tenor_search(query: String, pos: Option<String>) -> Result<TenorSearchResult, String>
#[tauri::command] pub async fn tenor_trending(pos: Option<String>) -> Result<TenorSearchResult, String>
#[tauri::command] pub fn get_gif_search_settings() -> GifSearchSettings
#[tauri::command] pub fn set_gif_search_settings(settings: GifSearchSettings) -> Result<(), String>
```

`TenorSearchResult`:
```rust
pub struct TenorSearchResult {
    pub gifs: Vec<TenorGif>,
    pub next: Option<String>,  // pagination cursor for follow-up calls
}

pub struct TenorGif {
    pub id: String,
    pub title: String,
    pub preview_url: String,  // tinygif format — small animated preview
    pub full_url: String,     // gif format — full quality for sending
    pub width: u32,
    pub height: u32,
}
```

`GifSearchSettings`:
```rust
pub struct GifSearchSettings {
    pub enabled: bool,                          // default false
    pub content_filter: String,                  // "high" | "medium" | "low" | "off"; default "high"
    pub user_api_key: Option<String>,            // None → use embedded default key
}
```

Stored under `~/.farder/settings.json` keys: `gif_search_enabled`, `gif_search_content_filter`, `gif_search_user_key`.

### API key resolution

Order of preference:
1. `gif_search_user_key` from settings (if set + non-empty)
2. Embedded `TENOR_DEFAULT_KEY` const in `tenor.rs`

If both fail (auth error from Tenor), surface a structured error to the UI: *"Your API key was rejected. Check it in Settings → GIF Search."*

If quota is exceeded (429 or quota error response): *"GIF search is over quota. Try setting your own API key in Settings → GIF Search."*

The embedded key ships in the source. Rotation requires a Rust rebuild. For v1, the dev (you) generates a real free Google Cloud API key for the embedded value. Production builds may want to swap it via build env var — not in scope for v1.

### Privacy gate

Every Tenor query checks `gif_search_enabled` first. If false, return `Err("GIF search is not enabled")`. The frontend wraps the picker open with a check so the modal never even tries to query Tenor when disabled.

The 🎬 button in the message input row is **always visible** (Discord-style discoverability). On first click when `enabled === false`, the `GifSearchOptIn` modal opens with the privacy warning + Enable / Cancel. On Enable, the setting flips to true, modal closes, picker opens. Subsequent clicks open the picker directly.

Disabling lives in Settings → GIF Search → toggle off.

### Sending a GIF

Flow when user clicks a result tile:
1. Picker calls `fetch_url(serverId, full_url, channelId)` — existing Tauri command that downloads the URL bytes and uploads them to the target server's attachment store, returning a `file_id`.
2. Picker calls `send_message(serverId, channelId, "", undefined, [file_id])` — empty content + the GIF as the only attachment.
3. Picker closes.

Receiver side: just sees a normal image attachment from the server's storage. Renders via the existing `AttachmentDisplay`. No mention of Tenor anywhere on the wire.

### Saving a GIF to the book

Hover overlay "📚" button on each tile:
1. Calls `fetch_url(serverId, full_url, channelId)` to get the file_id (file is now stored on the current server).
2. Calls `book_save_from_url(serverId, file_id, gif.title)` — the existing book save flow from Phase 1, which downloads the bytes (already cached server-side from the fetch_url) and writes them to the book.

After saving, the book item has `server_files[serverId] = file_id` cached, so future uses on this same server skip re-upload entirely. Net cost: one server upload + one client-side download for the save action.

### NSFW handling

Every Tenor query passes `contentfilter=<setting>`. Default `high`. User-adjustable to `medium` / `low` / `off` in Settings. Switching to "off" triggers a confirm prompt: *"Content filter off — adult content may appear. Are you sure?"*. The picker shows a small warning banner at the top whenever the filter is below "high".

### Search behavior

- **Empty input on picker open:** auto-fetch trending (one `tenor_trending` call). Display trending GIFs as the default state.
- **User types in search:** debounce 300ms after last keystroke, then call `tenor_search`.
- **In-flight requests:** when a new keystroke comes in while a request is in-flight, the JS side ignores the old response (no AbortController needed; just track a request seq number and check on response).
- **Pagination:** scrolling to the bottom of the result grid triggers a follow-up call with the previous response's `next` cursor. Append results.

## Components

**New TS:**
- `client/src/components/GifPicker.tsx` — popover above the 🎬 button. Search input + trending/results grid + scroll-pagination + hover-save overlay.
- `client/src/components/GifSearchOptIn.tsx` — small modal with privacy warning + Enable / Cancel buttons.
- `client/src/components/GifSearchSettings.tsx` — content for the new "GIF Search" settings tab.

**Modified TS:**
- `client/src/components/MessageInput.tsx` — add the 🎬 button next to existing 🎁 / 📚 / etc. State for `showGifPicker` + `showOptIn`. On click, branches based on `gif_search_enabled` setting.
- `client/src/components/AppearanceSettings.tsx` — rename to `SettingsModal.tsx` (or keep filename, add a tab structure); add a "GIF Search" tab alongside the existing appearance content. Rename the dialog title from "Appearance" to "Settings". (User-facing rename — the file rename is optional but consistent.)

**New Rust:**
- `client/src-tauri/src/tenor.rs` — Tenor API client + 4 commands.
- `client/src-tauri/Cargo.toml` — confirm `reqwest` is already a dep (used elsewhere) and pull in JSON features if not present.

**Modified Rust:**
- `client/src-tauri/src/main.rs` — register the 4 new Tenor commands.

**TS bridge bindings (in `client/src/lib/tauri-bridge.ts`):**
```ts
tenorSearch(query, pos?): Promise<TenorSearchResult>
tenorTrending(pos?): Promise<TenorSearchResult>
getGifSearchSettings(): Promise<GifSearchSettings>
setGifSearchSettings(s): Promise<void>
```

Plus `TenorGif`, `TenorSearchResult`, `GifSearchSettings` types in `lib/types.ts`.

## Settings UI

The existing `AppearanceSettings` modal is the only general-settings surface today. Phase 2 of this feature renames it to "Settings" (user-facing) and adds a tab structure:

- **Appearance** (existing content — themes picker, customizer entry)
- **GIF Search** (new — `GifSearchSettings` content)

GIF Search tab contents:
- Heading: "GIF Search"
- Toggle: "Enable Tenor GIF search" (controls `enabled`)
- Sub-paragraph (when enabled): "Tenor is owned by Google. Searches are sent to Google's servers; your IP and search terms are visible to them."
- Dropdown: "Content filter" (high/medium/low/off; default high)
- Text input: "Your Tenor API key (optional)" with placeholder "leave blank to use Farder's default" + a help link to Tenor's developer portal
- Footer link: "How to get a Tenor API key" (opens Google's docs page via `tauri-plugin-shell` open)

Save behavior: every input fires `setGifSearchSettings` immediately on change (no save button). Each call writes the whole struct to settings.json.

## Edge cases

- **No network on Tenor query:** picker shows error message, no crash.
- **Quota exceeded:** picker shows the "set your own key" message.
- **Invalid user key:** picker shows the "key rejected" message; user navigates to settings to fix.
- **NSFW filter "off":** picker shows persistent warning banner at the top.
- **Empty trending response:** picker shows "Try a search to find GIFs" placeholder.
- **Picker opened without `enabled`:** opt-in modal opens instead.
- **Settings stored before this feature:** `enabled` defaults to false, `content_filter` defaults to "high", `user_api_key` defaults to None — no migration needed (settings.json missing keys are interpreted as defaults via the existing `settings_get` helper).
- **Tenor returns content other than GIFs:** we request `media_filter=tinygif,gif` so only those formats come back.

## Backwards compatibility

No protocol changes. Sent GIFs are regular image attachments — old clients render them normally without ever knowing they came from Tenor.

## Success criteria

- 🎬 button visible in the message input on a fresh install.
- First click → opt-in modal with privacy warning. Cancel → no setting change, picker doesn't open.
- Click Enable → setting flips to true, picker opens, trending GIFs visible.
- Search input filters results within ~500ms of last keystroke.
- Click a result → message sends as a normal image attachment (visible in chat as an image, not a Tenor link).
- Hover a result → "📚" overlay appears; click → GIF added to the book.
- Scroll to bottom of results → next page loads.
- Settings → GIF Search shows the toggle, content filter dropdown, and BYO-key input.
- Setting "Content filter" to "off" requires a confirm; warning banner appears in the picker.
- Quota error / network error / invalid-key error each surface a clear message in the picker.
- Recipients of sent GIFs (with GIF search disabled, or on old clients) see them as normal image attachments.

## Out of scope / deferred

- **Trending category browser** (Tenor's category endpoint). Could add as a "Browse" tab in the picker later.
- **Recent searches history.** Defer.
- **Saved GIFs collection** (separate from book). Use the book — that's why it exists.
- **Animated thumbnails toggle** (e.g. for users who want static previews to save bandwidth). Defer.
- **Tenor's "Locale" parameter** for region-specific results. Default to en_US for v1.
- **Multi-language search.** Tenor's search is mostly English-centric in practice.
- **Build-time API key injection via env var.** Embedded source-level for v1; production rotation is a separate concern.

## Implementation notes (non-binding, for the planner)

- **`reqwest` is already a dep** of the Tauri client crate (used by other modules); just enable the `json` feature if not already on.
- **Tenor's API base URL:** `https://tenor.googleapis.com/v2/`. Search endpoint: `/search?q=<query>&key=<key>&contentfilter=<filter>&media_filter=tinygif,gif&pos=<cursor>`. Trending: `/featured?key=<key>&contentfilter=<filter>&media_filter=tinygif,gif&pos=<cursor>`. Both return `{ results: [...], next: "..." }`.
- **Each Tenor result has `media_formats.tinygif` and `media_formats.gif`** with `url`, `dims: [w, h]`, etc. Map those into our `TenorGif` shape.
- **Settings tab pattern:** look at how the BannedMembersTab integration was done in ServerSettingsDialog (commit `efbd761`) — same pattern (active tab state + conditional render). The user-footer ⚙ button currently opens AppearanceSettings; that becomes the new tabbed Settings modal.
- **The book-save flow** uses `fetch_url` (existing) → `book_save_from_url` (existing). Both already handle the cross-server file upload. Reuse — don't introduce a new "save external URL" path.
