# Translation v1.1 — Design (Manual Picker + Per-User Override)

**Status:** Drafted 2026-05-23
**Scope:** Farder client (Tauri + React). No server or protocol changes. Extends Translation v1.

## Goal

Two UX improvements on top of Translation v1:

1. **Better manual language picker.** The current low-confidence fallback uses `window.prompt()` and asks the user to type a 2-letter ISO code. Replace with a real dropdown listing the user's installed source languages + an "Add a new language…" option that opens the existing model-download flow inline.

2. **Per-user language override.** Right-click a member → "Set language…" → pick a language. After that, every message from that user skips auto-detection and routes directly to the chosen source language. Stored locally per public key. Useful when franc keeps miscalling a particular user's messages (which is common for short messages, non-Latin scripts, or unusual phrasings).

## Non-Goals

- **Server-side language hints.** Some platforms store a user's "primary language" on the server. We don't — overrides are purely client-local.
- **Auto-detection banner under every message.** The original Phase B sketch included this. Dropped as redundant: detection already runs when the user right-clicks Translate, and surfacing a banner before that requires solving the same detection-reliability problem.
- **Cross-device sync of overrides.** Local-only — overrides live in `~/.farder/settings.json` on each client.
- **Override propagation across server changes.** Public keys are global identity; an override set in one server applies to that user in every server you share with them. (This is correct, not a bug.)

## Architecture

```
┌─────────────────────────────────────── Tauri WebView ───────────────────────────────────────┐
│                                                                                              │
│  MemberContextMenu  (right-click member)                                                     │
│    └─ existing items: View Profile · Send Message · Assign Roles · …                         │
│    └─ NEW: "Set language…" — opens an inline submenu of installed langs +                    │
│                              "Add a new language…" entry                                     │
│                                                                                              │
│  TranslatedRow  (under each foreign message)                                                  │
│    └─ low-confidence state — RE-WIRED:                                                       │
│        from: prompt("Source language code (en, es, …)?")                                     │
│        to:   inline <SourceLanguagePicker /> dropdown component                              │
│                                                                                              │
│  client/src/components/SourceLanguagePicker.tsx  (NEW, shared)                               │
│    └─ Dropdown listing:                                                                      │
│        ├─ Installed languages (from listLocalModels)                                          │
│        ├─ "──────"                                                                            │
│        └─ "Add a new language…" → opens TranslationDownloadDialog                            │
│                                                                                              │
│  client/src/lib/translation/store.ts                                                          │
│    └─ translateMessage() — NEW: checks per-user override FIRST,                              │
│                            skips detect() if override present                                │
│                                                                                              │
│  Storage:                                                                                     │
│    Rust `get_translation_settings` extended with                                              │
│      user_language_overrides: Record<publicKeyHex, iso1>                                     │
│    Persisted via existing `settings_set("translation_user_overrides", …)`                   │
│                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

## UX

### "Set language…" in member context menu

Inserted between **Copy mention** (last item today) and the danger items (Kick/Ban — they're shown conditionally above). Position: just after **Copy mention**, before nothing (it becomes the new last item).

```
┌─────────────────────────────────┐
│  View Profile                   │
│  Send Message                   │
│  Assign Roles ▸                 │
│  Kick                           │  (perm-gated)
│  Ban                            │  (perm-gated)
│  Block                          │
│  Copy ID                        │
│  Copy mention                   │
│  Set language… ▸  ◀ NEW         │
└─────────────────────────────────┘
```

Hovering "Set language…" opens a submenu:

```
┌─────────────────────────────────┐
│  ✓ Spanish                      │ (currently assigned — checked)
│    French                       │
│    German                       │
│    Japanese                     │
│    ─────────                    │
│    Clear override               │ (only if one is set)
│    Add a new language…          │ (opens TranslationDownloadDialog)
└─────────────────────────────────┘
```

- Listed langs come from `listLocalModels()` filtered to those where the user's `default_target` is the *target*. (e.g., if your target is "en", we show languages X where the X→en model is installed.)
- A checkmark marks the currently-assigned language for this user (read from settings).
- "Clear override" removes the entry from `user_language_overrides`.
- "Add a new language…" opens the same `TranslationDownloadDialog` used elsewhere; after download completes, the dialog closes and the new language is auto-assigned.

### Improved low-confidence picker in TranslatedRow

Today's flow (problem):
```
Couldn't detect language. [Pick source…]   ← clicking shows window.prompt()
```

New flow:
```
Couldn't detect language. Source: [English ▾]  [Translate]
                                  ↓ click
                                  English
                                  Spanish (en)
                                  French (fr)
                                  ─────────
                                  Add a new language…
```

- Same `<SourceLanguagePicker />` component as the member submenu.
- Selecting a language and clicking **Translate** calls `translateMessageWithSource({ src, …})`.
- "Add a new language…" opens the download dialog inline; after download, the new language becomes the picker's selection automatically.

### When override exists, detection is skipped

If the user has an override set for the message author's public key, `translateMessage()`:
1. Skips `detect()` entirely.
2. Goes straight to the `loading-model → translating → done` path with the assigned `src`.
3. If the model isn't installed (somehow — they cleared it after setting), surface the download dialog.

## Component contract

### `<SourceLanguagePicker />`

```tsx
interface Props {
  /** Currently selected ISO 639-1 code (or null for "no selection yet"). */
  value: string | null;
  onChange: (iso1: string) => void;
  /** Optional: rendered as a row in the menu when present (Clear override). */
  onClear?: () => void;
  /** If true, render as a `<select>` styled like the surrounding theme.
   *  If false, render as a submenu-style vertical list of `<div>`s (used
   *  inside the member context menu). */
  variant: "select" | "menu";
}
```

The component fetches installed models via `listLocalModels()` on mount + when an "Add language" download completes. The list is filtered to languages where the *target* is the user's `default_target`.

## Storage

### Settings additions

**Rust** (`translation.rs`):

```rust
pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,
    pub seen_first_run: bool,
    /// Per-user language overrides, keyed by ISO 639-1 codes.
    /// e.g. `{ "abc123hex": "ja" }` means "messages from member with
    /// pubkey abc123hex are always treated as Japanese source".
    pub user_language_overrides: HashMap<String, String>,
}
```

Persisted as `settings_set("translation_user_overrides", JSON_object)`.

**TypeScript** (`types.ts`):

```ts
export interface TranslationSettings {
  enabled: boolean;
  default_target: string;
  seen_first_run: boolean;
  user_language_overrides: Record<string, string>;
}
```

Field name is `user_language_overrides` (snake_case) to match the Rust serialization. The codebase already uses snake_case for other fields like `default_target` and `seen_first_run`, so this is consistent.

### Override lookup

The store reads overrides on `translateMessage(opts)` entry:

```ts
const settings = await getTranslationSettings();
const override = settings.user_language_overrides[opts.authorPublicKeyHex];
if (override) {
  // Skip detect, use override directly
  return translateMessageWithSource({ ...opts, src: override });
}
// existing detect + translate path
```

This requires extending `TranslateOptions` with a new field `authorPublicKeyHex: string` — `Message.tsx` already has the public key via `message.author.bytes` and `publicKeyToString()`.

## Data flow

### Right-click → Set language → pick

1. User right-clicks a member name (in sidebar OR in chat).
2. `MemberContextMenu` opens with the new "Set language…" item.
3. Hover → submenu opens. `<SourceLanguagePicker variant="menu" />` renders inside.
4. User clicks an installed language → `onChange("es")` fires → settings updated → submenu + outer menu close.
5. Next time that user sends a message and the recipient clicks Translate → `translateMessage` reads the override, skips detection, runs directly.

### Right-click → Translate (no override, low confidence)

1. User right-clicks a message → Translate.
2. `translateMessage` runs `detect()` → returns low confidence.
3. Store transitions to `{ kind: "low-confidence", suggested: "fr" }`.
4. `TranslatedRow` renders the new `<SourceLanguagePicker variant="select" />` defaulting to `suggested` if present.
5. User picks French → clicks Translate → `translateMessageWithSource({ src: "fr" })` runs.

### Add a new language inline

1. From either picker entry point, user clicks "Add a new language…".
2. `TranslationDownloadDialog` opens (the same one used in the right-click flow today).
3. After successful download, dialog closes; the picker's `<select>` value is set to the newly-downloaded language and `onChange` fires.

## Error handling

| Scenario | Behavior |
|---|---|
| Override set but model uninstalled (user deleted it from Settings) | `ensureModel()` triggers the download dialog as it would for any missing model. No special path. |
| User clears override while a translate is in-flight | The in-flight request finishes with the previous override; next click re-detects normally. |
| Picker's "Add a language" dialog cancelled | Picker selection stays at its previous value; no state corruption. |
| Override's iso1 code is no longer in the language map (corrupted settings) | Treat as "no override" — fall through to detection. |

## Testing

### Unit (Rust)

- `get_translation_settings` returns an empty `user_language_overrides` map by default (settings.json missing the key).
- `set_translation_settings` round-trips the map (add an entry, persist, reload, entry survives).

### Manual smoke

- Set an override for one user → their next message gets translated with the assigned language, skipping detection.
- Clear the override → next message goes through detection again.
- Pick "Add a new language" → download dialog opens, completes, new language is selected.
- Low-confidence picker shows installed languages first; "Add a new language" works the same way.
- Long Spanish text → detected, translated directly (no picker).
- Short ambiguous text → low-confidence picker shows; choose a language → translated.

## File inventory

**Created:**
- `client/src/components/SourceLanguagePicker.tsx` — shared picker component
- `client/src/components/SetLanguageSubmenu.tsx` — submenu glue for MemberContextMenu (kept separate since the menu has its own layout primitives)

**Modified:**
- `client/src-tauri/src/translation.rs` — extend `TranslationSettings` with `user_language_overrides`, update `get/set_translation_settings` to persist
- `client/src/lib/translation/types.ts` — extend `TranslationSettings` interface
- `client/src/lib/translation/store.ts` — `translateMessage` checks override first, skips detect; `TranslateOptions` gains `authorPublicKeyHex`
- `client/src/components/Message.tsx` — pass `authorPublicKeyHex` to `translateMessage`
- `client/src/components/TranslatedRow.tsx` — replace `prompt()` with `<SourceLanguagePicker variant="select" />`
- `client/src/components/MemberContextMenu.tsx` — add "Set language…" entry that opens the submenu

## Rollout

- No data migration (the new settings field defaults to empty).
- Backward-compatible with v1: existing translations keep working; overrides are purely additive.
- No server changes.

## Future (v1.2+)

- Surface overrides in Settings → Translation as a viewable/editable list ("Per-user overrides: 3 users assigned").
- Quick-set-from-current-message: in the TranslatedRow's manual picker, an "Always use \<lang\> for \<author\>" checkbox that creates the override in one click.
- Server-side hint: per-server admin can mark "this server's primary language is X" so the source default differs from the user's default_target. (Out of scope here; needs protocol change.)
