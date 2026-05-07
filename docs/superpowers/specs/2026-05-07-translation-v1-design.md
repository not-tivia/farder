# Message Translation v1 — Design

**Status:** Approved 2026-05-07
**Scope:** Farder client (Tauri + React). No server changes (no protocol additions). Server-side translation deferred to v1.5.

## Goal

Right-click any chat message → "Translate" → translated text appears below the original. All translation runs on the user's device via Bergamot WASM (the same engine Firefox Translate uses); no chat content ever leaves the device. Works in DMs and channels equally, on any server.

## Non-Goals

- **Server-side translation.** Deferred to v1.5; servers may opt in to translate channel messages once. DMs will always stay client-side regardless (E2E encryption means the server can't see plaintext).
- **Auto-translate per-channel.** Deferred to v1.5. v1 is right-click-on-demand only.
- **Hover-over-any-text translation.** Janky to detect text under cursor across heterogeneous components. Out of scope.
- **Translate-on-send** (compose in your language, recipient sees their language). Out of scope; recipient pulls translation themselves on demand.
- **Disk-persistent translation cache.** v1 uses an in-memory per-session cache. Re-translating a message after app restart is acceptable.
- **Floating Tauri WebView proxying translate.google.com.** Janky, leaks every translation to Google, dismissed in research.
- **macOS in v1.** Linux + Windows targets only (matches the existing voice-calling cfg-gating decision; macOS support to be revisited later).

## Architecture

```
┌────────────────────────── Tauri WebView (renderer) ─────────────────────────┐
│                                                                              │
│  Message component                                                           │
│    └─ right-click menu ─→ "Translate" (single item)                          │
│    └─ TranslatedRow (below original)                                         │
│                                                                              │
│  client/src/lib/translation/                                                 │
│    ├─ detect.ts         (franc wrapper — source language detection)          │
│    ├─ engine.ts         (loads bergamot WASM, holds Translator instances)    │
│    ├─ models.ts         (Mozilla registry fetcher + cache + integrity check) │
│    └─ store.ts          (translation state per message)                      │
│                                                                              │
│  Settings → "Translation" tab (added to existing Settings modal)             │
│    ├─ default target language picker                                         │
│    ├─ installed models list (with size, delete buttons)                      │
│    └─ "Add language" — fetch model from registry                             │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ Tauri IPC (filesystem + registry fetch)
                                    ▼
┌──────────────────────── Rust side (client/src-tauri) ───────────────────────┐
│                                                                              │
│  src-tauri/src/translation.rs                                                │
│    ├─ #[command] list_local_models() -> Vec<LocalModel>                      │
│    ├─ #[command] download_model(pair) -> () (streams from Mozilla GCS)       │
│    ├─ #[command] get_model_paths(pair) -> ModelPaths (asset:// URLs)         │
│    ├─ #[command] delete_model(pair) -> ()                                    │
│    ├─ #[command] get_translation_settings() -> TranslationSettings           │
│    └─ #[command] set_translation_settings(settings) -> ()                    │
│                                                                              │
│  Storage: ~/.farder/translation-models/<src>-<trg>/                           │
│    ├─ model.bin         (decompressed, ready for bergamot)                   │
│    ├─ vocab.spm         (sentencepiece vocabulary)                           │
│    ├─ lex.bin           (lexical shortlist)                                  │
│    └─ meta.json         (version, sha256, downloaded_at)                     │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Why WASM-in-WebView, not native FFI

Bergamot is a C++ library; embedding it natively in `farder-client` requires emscripten + cmake at build time. We just hit that exact problem with `audiopus_sys` and parked Phase 3 of voice. WASM-in-WebView avoids the entire native-build hill: the npm package ships a pre-compiled `.wasm` blob (~5–10 MB) that runs in the existing Tauri WebView. The Rust side becomes a thin filesystem helper — download/save/load/delete — using `reqwest` (already a dep) and `dirs`.

### Why the Rust side handles model download

The renderer is sandboxed; writing 50 MB blobs to `~/.farder/translation-models/` would require either Tauri's `fs` plugin with broad permissions, or chunking to base64 over IPC. Routing the download through a `#[tauri::command] download_model(pair)` command keeps the renderer focused on UI and lets us stream-decompress server-side.

Model bytes also do **not** flow back through IPC for instantiation — that would mean ~40 MB JSON-serialized per language pair on first translate. Instead the renderer asks for `ModelPaths` (file URLs from Tauri's asset protocol via `convertFileSrc`) and Bergamot WASM `fetch()`-es them like any browser asset. This keeps IPC payloads tiny.

### Tauri IPC type sketches

```rust
pub struct LangPair { pub src: String, pub trg: String }  // ISO 639-1 codes

pub struct LocalModel {
    pub pair: LangPair,
    pub disk_size_bytes: u64,
    pub downloaded_at: u64,
    pub version: String,
}

pub struct ModelPaths {
    pub model: String,    // asset:// URL or convertFileSrc-resolved file URL
    pub vocab: String,
    pub lex: String,
}

pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,    // ISO 639-1
    pub seen_first_run: bool,
}
```

Types live in `src-tauri/src/translation.rs` and are re-exported via the existing TS bindings pattern (matching how `tenor.rs` exposes `TenorSearchResult` etc.).

### Why models aren't bundled

Three reasons:
1. **Installer size:** bundling en/es/zh adds ~150–180 MB to the installer (~3-4× current size).
2. **First-run modal lets users opt out:** "skip for now" demotes to fetch-on-first-translate. Users who never translate never download anything.
3. **Mozilla's GCS bucket is the canonical source.** Mirroring inside our installer means we'd need to re-bundle on every model update.

## Translation engine

### Bergamot WASM

- npm package: [`@browsermt/bergamot-translator`](https://www.npmjs.com/package/@browsermt/bergamot-translator) v0.4.9. Last published 2022-10. Maintenance mode but functional; Firefox uses the same engine in production. License: MPL-2.0.
- The WASM binary ships in the npm package and is loaded once on app start (lazy on first translate). Subsequent translations don't reload.
- Two translator modes: `LatencyOptimisedTranslator` (interactive, used here) and `BatchTranslator` (bulk; not needed for v1).
- One Translator instance per language pair, kept in `Map<"src-trg", Translator>` for the session. First translation in a pair: ~500 ms model load + translate. Subsequent: <100 ms typically.

### Source language detection (`franc`)

- npm package: [`franc-min`](https://www.npmjs.com/package/franc-min) (~150 KB minified, no native deps). Returns ISO 639-3 codes — we map to ISO 639-1 (the Bergamot model identifiers) via a small static table.
- Used on every translate click. If the detected source matches the user's target language, we render `"Already in <Language>"` instead of attempting translation.
- Confidence threshold: franc returns a probability score per language; below 0.5 we surface a small "Pick source language" dropdown in the translation row.
- Future-proofing: behind a thin `detect()` interface so we can swap to `lingua-rs` (compiled to WASM) later if franc accuracy disappoints.

### Model registry

- Source: `https://storage.googleapis.com/moz-fx-translations-data--303e-prod-translations-data/db/models.json` (verified responsive, public, cacheable, returns 107 model entries).
- Each entry has `sourceLanguage`, `targetLanguage`, `architecture`, `releaseStatus`, `files.{model,vocab,lexicalShortlist}.path`, file `uncompressedSize`, and `uncompressedHash` (SHA-256). We pick the entry where `releaseStatus == "Release"` and prefer `architecture == "base-memory"` when available (lower memory footprint).
- Models are gzipped on the bucket; we decompress in Rust during download.

## UX

### First-run modal

On first app launch (i.e. no `translation_settings` key in `~/.farder/settings.json`), present a one-time modal:

> **Translation**
>
> Farder can translate messages between languages, on your device — no chat content is ever sent anywhere.
>
> Pick the languages you want to translate to and from. Each is ~50 MB and is downloaded from Mozilla's servers.
>
> ☐ English
> ☐ Spanish
> ☐ Chinese
>
> [Skip — I don't need translation right now] [Download selected]

"Skip" sets `translation_settings.enabled = false`. The right-click menu hides the Translate item. Re-enabling later is via the Settings → Translation tab.

### Right-click → Translate

The existing message context-menu in `client/src/components/Message.tsx` (around the `setMenu` block) gains a new entry:

```
Reply
Edit       (own messages)
Translate  ← new
Delete     (own messages)
Create Thread
```

Click → store transitions through `idle → detecting → loading-model → translating → done | error | already-in-target`.

### TranslatedRow

Shows immediately below the message bubble:

```
┌─────────────────────────────────────────────────┐
│  [translated text]                  [×]         │
│  ↳ Translated from Spanish · click to retry     │
└─────────────────────────────────────────────────┘
```

- Muted text color (uses the existing `--text-muted` CSS variable from the theme system).
- `[×]` removes the translation row for that message (in-memory; doesn't persist).
- During load, shows a spinner with state-specific copy: "Detecting language…", "Downloading Spanish model (32 MB)…", "Translating…".
- On error: "Translation failed — retry?" with a retry icon.
- On `already-in-target`: muted "Already in English" line — no retry button.

### Privacy disclosure

When the user triggers a model download (either via the first-run modal or via the right-click flow when the model isn't cached), they see a one-time-per-language confirmation dialog:

> **Download translation model?**
>
> Downloading the Spanish↔English models (~50 MB) from Mozilla's servers (storage.googleapis.com).
>
> Mozilla will see your IP address and the language pair you're requesting.
>
> Once downloaded, all translation runs entirely on your device — no chat content is ever sent anywhere.
>
> [Cancel] [Download]

After the user confirms a given pair once, future downloads of the *same pair* (e.g., re-download after deletion) skip the dialog. New language pairs always show it.

### Settings → Translation tab

Added to the existing tabbed Settings modal alongside Appearance + GIF Search:

```
Translation
─────────────────────────────────────────────────────
Default target language: [English ▾]

Installed languages
  • English (built-in detection)
  • Spanish ↔ English  (98 MB)  [Delete]
  • Chinese ↔ English  (84 MB)  [Delete]

[+ Add language]   ← opens picker → triggers download flow
```

A toggle at the top of the Translation tab — "Enable translation" — turns the feature off without uninstalling models. When off, the right-click "Translate" item is hidden and the first-run modal won't reappear.

## Data flow

1. User right-clicks message → menu → "Translate".
2. `Message.tsx` calls `store.translate(messageId, content)`.
3. Store checks in-memory cache (keyed by `messageId`); on hit, set state and return.
4. `detect(content)` (franc wrapper) → returns ISO 639-1 source code.
5. If detected source equals target → state becomes `already-in-target`.
6. Else: `engine.ensureModel(src, trg)`:
   - Calls `list_local_models` IPC; if pair present → calls `get_model_paths` → Bergamot WASM `fetch()`-es each file URL → instantiates `Translator` and stores it in the per-session map.
   - Else: shows download confirmation dialog → calls `download_model(pair)` IPC → progress reported via Tauri event channel → on success, instantiate Translator.
7. `engine.translate(content, src, trg)` → returns translated text.
8. Store updates with `{status: "done", text, src, trg, detectedConfidence}`. `TranslatedRow` renders.

## Error handling

| Failure | Behavior |
|---|---|
| Mozilla CDN unreachable / 5xx | Download dialog surfaces a Retry button + "Try again later". Already-downloaded models keep working. |
| SHA-256 mismatch on downloaded file | Delete partial download, surface "Model file corrupted — retry?" |
| WASM init failure (e.g., browser disabled WASM) | Translate menu item disabled with a tooltip "Translation engine failed to load". Logged once. |
| `franc` confidence < 0.5 | TranslatedRow shows "Couldn't detect source language" with a small dropdown to pick manually. |
| `engine.translate()` throws | TranslatedRow shows "Translation failed — retry?" with retry icon. The translation row doesn't break the underlying Message render. |
| Disk full when saving model | Roll back partial files, surface clear error. |
| Detected source language has no model in Mozilla's registry | TranslatedRow shows "<Language> isn't supported". One-time dialog; no retry. |

All errors are logged via the existing `console.error` + Tauri log path; nothing surfaces as an uncaught render exception.

## Testing

### Unit (TypeScript)
- `detect.ts`: fixture strings in 5+ languages return correct ISO 639-1 codes; low-quality input (1-2 words) returns a low-confidence sentinel.
- `store.ts`: state transitions through {idle → detecting → loading-model → translating → done} cleanly; errors at each stage produce the right user-facing state; in-memory cache hits skip work.
- `models.ts`: registry parser picks the right entry given multiple architectures (`base` vs `base-memory`); falls back gracefully when `releaseStatus != "Release"`.

### Unit (Rust)
- `download_model`: writes are atomic (`*.tmp` then rename), so an interrupted download leaves no partial files behind.
- `delete_model`: removes the entire pair directory recursively (`fs::remove_dir_all`); idempotent if already gone.
- `list_local_models`: skips directories without a valid `meta.json`.
- `get_model_paths`: returns asset URLs that resolve to existing files; returns 404-equivalent error when pair not present.

### Integration
- Mock the Mozilla registry endpoint with a tiny test fixture; end-to-end "click translate → fetch → translate" using a stubbed Translator that echoes input. Verifies wiring without pulling real models.

### Manual smoke
- Existing two-client setup: Alice (`npm run tauri dev`) + Bob (`FARDER_DATA=/tmp/farder-bob` release binary).
- Bob sends a Spanish message → Alice clicks "Translate" → English appears below within a few seconds (longer on first language-pair download).
- Test offline: kill network mid-download, verify retry flow.
- Test "already in target": Alice translates Bob's English message → "Already in English" muted line, no API call.
- Test deletion: Settings → delete Spanish model → next translate triggers re-download dialog.

## Dependencies added

**Client (renderer):**
- `@browsermt/bergamot-translator@0.4.9` (~5 MB unpacked + WASM blob)
- `franc-min@^6` (~150 KB)

**Client (Rust):**
- `flate2 = "1"` — needed for gzip decompression of model files. Currently a transitive dep; needs explicit add.
- `reqwest` already a direct dep — used for streaming model downloads from Mozilla's GCS bucket.
- `sha2` already a direct dep — used for hash verification of downloaded files.

**Server:** none. No protocol changes.

## Migration / rollout

- No data migration required.
- First app launch after the feature lands shows the translation first-run modal. Users who skip have zero footprint added.
- Settings tab is purely additive to the existing tabbed Settings modal — no impact on Appearance or GIF Search.

## Future work (v1.5+)

- **Server-side translation.** Optional server feature; admin opts in. Server adds an `Argos` or `LibreTranslate` sidecar (Python or Docker), advertises support via a new `ServerInfo.supports_translation` field. Clients prefer server-translation when available; fall back to local for DMs always.
- **Auto-translate per-channel.** Per-channel toggle in channel settings. When on, every incoming message in a non-target-language is auto-translated on render.
- **Disk-persistent translation cache.** Stash translations in SQLite alongside messages so re-opening a chat doesn't re-translate.
- **Translation hover preview.** Hover over a foreign-language word for instant translation of just that word.
- **Upgrade detector** to `lingua-rs` (WASM build) if franc accuracy is unsatisfactory.
- **macOS support.** Bergamot WASM should work on macOS without changes; gate test/build to confirm.
