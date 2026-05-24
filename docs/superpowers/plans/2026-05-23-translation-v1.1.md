# Translation v1.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `prompt()`-based low-confidence language picker with a real dropdown, and add per-user language override (right-click member → "Set language…") so messages from a chosen user skip auto-detection.

**Architecture:** Pure UI + local settings. `TranslationSettings` gains a `user_language_overrides: HashMap<String, String>` field (publicKeyHex → ISO 639-1). `store.translateMessage` checks override before detection. A shared `<SourceLanguagePicker>` component lists installed languages + an "Add new" inline download flow.

**Tech Stack:** Rust (Tauri 2) + React 18 + TypeScript 5.5. No new npm deps.

**Spec:** `docs/superpowers/specs/2026-05-23-translation-v1.1-design.md`

---

## File structure

**Created:**
- `client/src/components/SourceLanguagePicker.tsx` — shared picker (select + menu variants)

**Modified:**
- `client/src-tauri/src/translation.rs` — extend `TranslationSettings` with `user_language_overrides`, update get/set
- `client/src/lib/translation/types.ts` — extend `TranslationSettings` interface
- `client/src/lib/translation/store.ts` — `translateMessage` checks override first; `TranslateOptions` gains `authorPublicKeyHex`
- `client/src/components/Message.tsx` — pass `authorPublicKeyHex` into translate flow
- `client/src/components/TranslatedRow.tsx` — replace `prompt()` with `<SourceLanguagePicker variant="select" />`
- `client/src/components/MemberContextMenu.tsx` — add "Set language…" entry with submenu

---

## Phase 1: Storage extension

## Task 1: Extend `TranslationSettings` with `user_language_overrides`

**Files:**
- Modify: `client/src-tauri/src/translation.rs`
- Modify: `client/src/lib/translation/types.ts`

### Step 1: Rust struct + persistence

Find (around line 33):
```rust
pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,
    pub seen_first_run: bool,
}
```

Replace with:
```rust
pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,
    pub seen_first_run: bool,
    /// Per-user language overrides, keyed by public-key hex string.
    /// Value is the ISO 639-1 source language to use for that user's
    /// messages, bypassing auto-detection.
    #[serde(default)]
    pub user_language_overrides: std::collections::HashMap<String, String>,
}
```

Find the `impl Default for TranslationSettings` block. Replace with:
```rust
impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_target: "en".to_string(),
            seen_first_run: false,
            user_language_overrides: std::collections::HashMap::new(),
        }
    }
}
```

In `get_translation_settings`, find the existing block that reads the three keys and constructs the struct. Just before the final `TranslationSettings { ... }` constructor, add:
```rust
    let user_language_overrides = crate::commands::settings_get("translation_user_overrides")
        .and_then(|v| serde_json::from_value::<std::collections::HashMap<String, String>>(v).ok())
        .unwrap_or_default();
```

Then extend the struct construction:
```rust
    TranslationSettings { enabled, default_target, seen_first_run, user_language_overrides }
```

In `set_translation_settings`, after the existing three `settings_set` calls, add:
```rust
    crate::commands::settings_set(
        "translation_user_overrides",
        serde_json::to_value(&settings.user_language_overrides)
            .map_err(|e| e.to_string())?,
    )?;
```

### Step 2: TS interface

Find (in `client/src/lib/translation/types.ts`):
```ts
export interface TranslationSettings {
  enabled: boolean;
  default_target: string;
  seen_first_run: boolean;
}
```

Replace with:
```ts
export interface TranslationSettings {
  enabled: boolean;
  default_target: string;
  seen_first_run: boolean;
  user_language_overrides: Record<string, string>;
}
```

### Step 3: Verify

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: both clean.

### Step 4: Commit

```
git -C /home/deez/farder add client/src-tauri/src/translation.rs client/src/lib/translation/types.ts
git -C /home/deez/farder commit -m "feat(client): TranslationSettings gains user_language_overrides"
```

Plus the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.

---

## Phase 2: Store wires override into translateMessage

## Task 2: store.ts — TranslateOptions extension + override lookup

**Files:**
- Modify: `client/src/lib/translation/store.ts`

### Step 1: Extend `TranslateOptions`

Find the existing `TranslateOptions` interface in `store.ts`. Add a new field:
```ts
  /** Hex public-key of the message author. Used to look up per-user language
   *  overrides; pass an empty string if not available (override lookup is skipped). */
  authorPublicKeyHex: string;
```

(Place it after `defaultTarget` — adjacent to other identity-ish fields.)

### Step 2: Override lookup at the top of `translateMessage`

Find the body of `translateMessage(opts: TranslateOptions)`. The current body starts:
```ts
  const { messageId, content, defaultTarget, confirmDownload } = opts;
  state.set(messageId, { kind: "detecting" });
  emit();

  const det = detect(content);
```

Replace with (note: pull in `getTranslationSettings` from `./api`):
```ts
  const { messageId, content, defaultTarget, confirmDownload, authorPublicKeyHex } = opts;

  // Check per-user language override first — if present, skip detection
  // and route directly through translateMessageWithSource.
  if (authorPublicKeyHex) {
    try {
      const settings = await getTranslationSettings();
      const override = settings.user_language_overrides[authorPublicKeyHex];
      if (override && override !== defaultTarget) {
        return translateMessageWithSource({
          ...opts,
          src: override,
        });
      }
      if (override && override === defaultTarget) {
        state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
        emit();
        return;
      }
    } catch (e) {
      // If settings fetch fails, fall through to detection — don't block
      // the user on a corrupt settings file.
      console.error("[translate] override lookup failed:", e);
    }
  }

  state.set(messageId, { kind: "detecting" });
  emit();

  const det = detect(content);
```

Add the import at the top of the file (alongside the existing imports):
```ts
import { getTranslationSettings } from "./api";
```

### Step 3: Verify TS compiles

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -8
```

Expected: there will be errors at `Message.tsx` call sites of `translateMessage` because `authorPublicKeyHex` is now required. That's expected — Task 3 fixes those callers.

### Step 4: Make `authorPublicKeyHex` required without breaking the build

For this commit to be self-contained (TS clean), do one of:

- Option A (chosen): make the new field optional in TS by adding `?:`, then in Task 3 the caller adds it. Final code uses it unconditionally with the empty-string guard.

Change Step 1's edit to:
```ts
  /** Hex public-key of the message author... */
  authorPublicKeyHex?: string;
```

And in Step 2's check use `if (authorPublicKeyHex) { … }` (already does).

Re-verify TS clean.

### Step 5: Commit

```
git -C /home/deez/farder add client/src/lib/translation/store.ts
git -C /home/deez/farder commit -m "feat(client): translate store checks per-user language override"
```

---

## Task 3: Message.tsx — pass authorPublicKeyHex into translateMessage

**Files:**
- Modify: `client/src/components/Message.tsx`

### Step 1: Find the translate-menu-item

Find the menu item that calls `translateMessage(...)`. The current callsite (Task 11 of v1) is roughly:
```tsx
await translateMessage({
  messageId: String(message.id),
  content: message.content,
  defaultTarget: translationSettings.default_target,
  confirmDownload: async (pair) =>
    new Promise<void>((resolve, reject) => {
      setPendingDownload({ pair, resolve, reject, inProgress: false });
    }),
});
```

Replace with the version that includes `authorPublicKeyHex`:
```tsx
await translateMessage({
  messageId: String(message.id),
  content: message.content,
  defaultTarget: translationSettings.default_target,
  authorPublicKeyHex: publicKeyToString(message.author),
  confirmDownload: async (pair) =>
    new Promise<void>((resolve, reject) => {
      setPendingDownload({ pair, resolve, reject, inProgress: false });
    }),
});
```

Verify `publicKeyToString` is already imported. If not, add:
```ts
import { publicKeyToString } from "../lib/types";
```

(It almost certainly is — the file already imports from `lib/types`.)

### Step 2: Update the `<TranslatedRow>` render call too

Find the existing `<TranslatedRow ... />` JSX. Its props (after Task 11 v1):
```tsx
<TranslatedRow
  messageId={String(message.id)}
  content={message.content}
  defaultTarget={translationSettings?.default_target ?? "en"}
  confirmDownload={async (pair) =>
    new Promise<void>((resolve, reject) => {
      setPendingDownload({ pair, resolve, reject, inProgress: false });
    })
  }
/>
```

Replace with the version that includes the author:
```tsx
<TranslatedRow
  messageId={String(message.id)}
  content={message.content}
  defaultTarget={translationSettings?.default_target ?? "en"}
  authorPublicKeyHex={publicKeyToString(message.author)}
  confirmDownload={async (pair) =>
    new Promise<void>((resolve, reject) => {
      setPendingDownload({ pair, resolve, reject, inProgress: false });
    })
  }
/>
```

(`TranslatedRow` will be updated to accept this prop in Task 5.)

### Step 3: Verify

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -8
```

There may be a TS error about `TranslatedRow` not accepting `authorPublicKeyHex` yet. That's expected; Task 5 will add the prop. To keep this commit self-contained, either:
- Skip the `<TranslatedRow>` change here and do both in Task 5, OR
- Add the prop to TranslatedRow now (will be revisited in Task 5).

**Choose:** do both `Message.tsx` callsite changes here. The TS error will surface; resolve by adding the prop to TranslatedRow as well (one-line interface addition, defer logic to Task 5).

In `TranslatedRow.tsx` find:
```tsx
interface Props {
  messageId: string;
  content: string;
  defaultTarget: string;
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}
```

Replace with:
```tsx
interface Props {
  messageId: string;
  content: string;
  defaultTarget: string;
  authorPublicKeyHex: string;
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}
```

(Don't use it yet — Task 5 wires it in.)

Re-run `tsc`. Should be clean now.

### Step 4: Commit

```
git -C /home/deez/farder add client/src/components/Message.tsx client/src/components/TranslatedRow.tsx
git -C /home/deez/farder commit -m "feat(client): thread authorPublicKeyHex to translate flow"
```

---

## Phase 3: SourceLanguagePicker component

## Task 4: Create `SourceLanguagePicker`

**Files:**
- Create: `client/src/components/SourceLanguagePicker.tsx`

### Step 1: Component

```tsx
// client/src/components/SourceLanguagePicker.tsx

import { useEffect, useState } from "react";
import { listLocalModels } from "../lib/translation/api";
import { displayName } from "../lib/translation/lang";
import type { LocalModel } from "../lib/translation/types";
import { TranslationDownloadDialog } from "./TranslationDownloadDialog";

interface Props {
  /** Currently selected ISO 639-1 source code, or null. */
  value: string | null;
  onChange: (iso1: string) => void;
  /** When set, renders a "Clear override" affordance that calls this. */
  onClear?: () => void;
  /** The target language; we filter installed models to those targeting this. */
  target: string;
  /** Visual mode: a styled <select> (inline use) or a vertical list of menu rows. */
  variant: "select" | "menu";
}

export function SourceLanguagePicker({ value, onChange, onClear, target, variant }: Props) {
  const [installed, setInstalled] = useState<LocalModel[]>([]);
  const [showDownload, setShowDownload] = useState<{ src: string; trg: string } | null>(null);

  async function refresh() {
    try {
      const all = await listLocalModels();
      setInstalled(all.filter((m) => m.pair.trg === target));
    } catch (e) {
      console.error("[source-picker] failed to list models:", e);
    }
  }

  useEffect(() => {
    refresh();
  }, [target]);

  const installedSources = installed.map((m) => m.pair.src).sort();
  const ADD_NEW_SENTINEL = "__add_new__";
  const CLEAR_SENTINEL = "__clear__";

  function handleAddNew() {
    // For v1.1 we ask the user to pick from the next-available source
    // language by prompt — minimal addition, mirrors today's flow at the
    // first-run modal. A future v1.2 would replace this with a full
    // available-pairs picker.
    const iso = window.prompt(
      `Enter an ISO 639-1 code to download (e.g., "ja" for Japanese, "ko" for Korean). Target is ${displayName(target)}.`,
    );
    if (!iso) return;
    const lower = iso.trim().toLowerCase();
    if (!/^[a-z]{2,8}$/.test(lower)) {
      window.alert(`Invalid language code: ${iso}`);
      return;
    }
    setShowDownload({ src: lower, trg: target });
  }

  function handleConfirmDownload() {
    if (!showDownload) return;
    setShowDownload({ ...showDownload });
    // The download dialog itself triggers the download via the
    // translation:progress event; we just wait for it to be "done".
    // For v1.1 we keep the flow simple: ask the dialog to call onConfirm,
    // then close it. (The real download is triggered by the dialog's
    // existing inProgress=true state after onConfirm fires.)
  }

  if (variant === "select") {
    return (
      <>
        <select
          value={value ?? ""}
          onChange={(e) => {
            const v = e.target.value;
            if (v === ADD_NEW_SENTINEL) {
              handleAddNew();
              return;
            }
            if (v === CLEAR_SENTINEL) {
              onClear?.();
              return;
            }
            if (v) onChange(v);
          }}
          style={{
            padding: "4px 8px",
            fontSize: 12,
            background: "var(--bg, #fff)",
            color: "var(--text, #000)",
            border: "1px solid var(--border, #ccc)",
            borderRadius: 4,
          }}
        >
          {value === null && <option value="">— pick a language —</option>}
          {installedSources.map((iso) => (
            <option key={iso} value={iso}>
              {displayName(iso)}
            </option>
          ))}
          <option disabled>──────</option>
          {onClear && value && (
            <option value={CLEAR_SENTINEL}>Clear override</option>
          )}
          <option value={ADD_NEW_SENTINEL}>Add a new language…</option>
        </select>
        {showDownload && (
          <TranslationDownloadDialog
            pair={showDownload}
            inProgress={false}
            onCancel={() => setShowDownload(null)}
            onConfirm={() => {
              // Mimic the existing download flow: set inProgress, then call
              // downloadModel from the dialog's perspective. We can't see
              // inside the dialog from here — simplest path: directly call
              // downloadModel and refresh on completion.
              (async () => {
                try {
                  const { downloadModel } = await import("../lib/translation/api");
                  await downloadModel(showDownload);
                  await refresh();
                  onChange(showDownload.src);
                } catch (e) {
                  console.error("[source-picker] download failed:", e);
                } finally {
                  setShowDownload(null);
                }
              })();
            }}
          />
        )}
      </>
    );
  }

  // variant === "menu"
  return (
    <div style={{ minWidth: 180 }}>
      {installedSources.length === 0 && (
        <div style={{ padding: "6px 12px", color: "var(--text-muted, #888)", fontSize: 12 }}>
          No source languages installed.
        </div>
      )}
      {installedSources.map((iso) => {
        const isSelected = iso === value;
        return (
          <div
            key={iso}
            onClick={() => onChange(iso)}
            style={{
              padding: "6px 12px",
              cursor: "pointer",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--accent-faded, rgba(0,88,230,0.12))")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <span style={{ width: 12 }}>{isSelected ? "✓" : ""}</span>
            <span>{displayName(iso)}</span>
          </div>
        );
      })}
      <div style={{ height: 1, background: "var(--border, #ccc)", margin: "4px 0" }} />
      {onClear && value && (
        <div
          onClick={onClear}
          style={{ padding: "6px 12px", cursor: "pointer", fontSize: 12, color: "var(--text-muted, #888)" }}
          onMouseEnter={(e) => (e.currentTarget.style.background = "var(--accent-faded, rgba(0,88,230,0.12))")}
          onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
        >
          Clear override
        </div>
      )}
      <div
        onClick={handleAddNew}
        style={{ padding: "6px 12px", cursor: "pointer", fontSize: 12, color: "var(--text-muted, #888)" }}
        onMouseEnter={(e) => (e.currentTarget.style.background = "var(--accent-faded, rgba(0,88,230,0.12))")}
        onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
      >
        Add a new language…
      </div>
      {showDownload && (
        <TranslationDownloadDialog
          pair={showDownload}
          inProgress={false}
          onCancel={() => setShowDownload(null)}
          onConfirm={() => {
            (async () => {
              try {
                const { downloadModel } = await import("../lib/translation/api");
                await downloadModel(showDownload);
                await refresh();
                onChange(showDownload.src);
              } catch (e) {
                console.error("[source-picker] download failed:", e);
              } finally {
                setShowDownload(null);
              }
            })();
          }}
        />
      )}
    </div>
  );
}
```

### Step 2: Verify

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

### Step 3: Commit

```
git -C /home/deez/farder add client/src/components/SourceLanguagePicker.tsx
git -C /home/deez/farder commit -m "feat(client): SourceLanguagePicker shared component"
```

---

## Phase 4: Wire the picker into UI

## Task 5: TranslatedRow uses SourceLanguagePicker

**Files:**
- Modify: `client/src/components/TranslatedRow.tsx`

### Step 1: Replace the `prompt()` low-confidence flow

Find the existing low-confidence render block:
```tsx
{status.kind === "low-confidence" && (
  <span>
    Couldn't detect language.{" "}
    <button
      onClick={() => {
        const src = prompt("Source language code (en, es, zh, …)?", status.suggested ?? "en");
        if (src) {
          translateMessageWithSource({
            messageId,
            content,
            src,
            defaultTarget,
            confirmDownload,
          });
        }
      }}
    >
      Pick source…
    </button>
  </span>
)}
```

Replace with:
```tsx
{status.kind === "low-confidence" && (
  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
    <span>Couldn't detect language. Source:</span>
    <SourceLanguagePicker
      variant="select"
      target={defaultTarget}
      value={status.suggested ?? null}
      onChange={(src) =>
        translateMessageWithSource({
          messageId,
          content,
          src,
          defaultTarget,
          authorPublicKeyHex,
          confirmDownload,
        })
      }
    />
  </div>
)}
```

Add at top of file:
```tsx
import { SourceLanguagePicker } from "./SourceLanguagePicker";
```

### Step 2: Pass `authorPublicKeyHex` to `translateMessageWithSource`

This requires updating `translateMessageWithSource` in `store.ts` too — it currently accepts `TranslateOptions & { src: string }`. Since `TranslateOptions` now includes the optional field, this should work automatically. Verify the call site in TranslatedRow.tsx compiles.

### Step 3: Verify

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -8
```

### Step 4: Commit

```
git -C /home/deez/farder add client/src/components/TranslatedRow.tsx
git -C /home/deez/farder commit -m "feat(client): TranslatedRow uses SourceLanguagePicker for low-confidence"
```

---

## Task 6: MemberContextMenu — "Set language…" submenu

**Files:**
- Modify: `client/src/components/MemberContextMenu.tsx`

### Step 1: Add the menu row

Find the `rows.push(...)` calls in `MemberContextMenu.tsx`. After `rows.push({ kind: "item", label: "Copy mention", onClick: copyMention });` (around line 220), add:

```tsx
  rows.push({
    kind: "submenu",
    label: "Set language…",
    onSubmenu: () => setShowLanguagePicker(true),
  });
```

Add a new `rows` kind variant near the type definition. The existing union (around line 188):
```tsx
type MenuRow =
  | { kind: "item"; label: string; onClick: () => void; danger?: boolean }
```

Extend with the new `submenu` variant:
```tsx
type MenuRow =
  | { kind: "item"; label: string; onClick: () => void; danger?: boolean }
  | { kind: "submenu"; label: string; onSubmenu: () => void }
```

(The existing `MenuRow` type may already be wider; if it's already a discriminated union with multiple kinds, just add the new variant alongside.)

### Step 2: Add the submenu state + handler

Inside the component (with other `useState` calls):
```tsx
  const [showLanguagePicker, setShowLanguagePicker] = useState(false);
  const [overrideValue, setOverrideValue] = useState<string | null>(null);
  const [defaultTarget, setDefaultTarget] = useState<string>("en");
```

Add an effect to load the current override + target on mount:
```tsx
  useEffect(() => {
    (async () => {
      const settings = await getTranslationSettings();
      setDefaultTarget(settings.default_target);
      const key = publicKeyToString(target.public_key);
      setOverrideValue(settings.user_language_overrides[key] ?? null);
    })();
  }, [target]);
```

Add imports at the top of the file:
```tsx
import { getTranslationSettings, setTranslationSettings } from "../lib/translation/api";
import { publicKeyToString } from "../lib/types";
import { SourceLanguagePicker } from "./SourceLanguagePicker";
```

(`publicKeyToString` may already be imported.)

### Step 3: Render the submenu

In the JSX, after the closing tag of the main menu but inside the same parent fragment, add:

```tsx
{showLanguagePicker && (
  <div
    style={{
      position: "absolute",
      // Position to the right of the main menu — adjust offset to taste
      top: 0,
      left: "100%",
      background: "var(--bg-elevated, #fff)",
      color: "var(--text, #000)",
      border: "1px solid var(--border, #ccc)",
      borderRadius: 4,
      boxShadow: "2px 2px 8px rgba(0,0,0,0.2)",
      zIndex: 1001,
      padding: 4,
    }}
  >
    <SourceLanguagePicker
      variant="menu"
      target={defaultTarget}
      value={overrideValue}
      onChange={async (src) => {
        const settings = await getTranslationSettings();
        const key = publicKeyToString(target.public_key);
        const next = {
          ...settings,
          user_language_overrides: {
            ...settings.user_language_overrides,
            [key]: src,
          },
        };
        await setTranslationSettings(next);
        setOverrideValue(src);
        setShowLanguagePicker(false);
        onClose();
      }}
      onClear={async () => {
        const settings = await getTranslationSettings();
        const key = publicKeyToString(target.public_key);
        const { [key]: _removed, ...rest } = settings.user_language_overrides;
        await setTranslationSettings({
          ...settings,
          user_language_overrides: rest,
        });
        setOverrideValue(null);
        setShowLanguagePicker(false);
        onClose();
      }}
    />
  </div>
)}
```

### Step 4: Handle the new `kind: "submenu"` row in the existing render loop

Find the existing `rows.map(...)` loop that renders each row. The current code renders only `kind === "item"`. Add handling for `kind === "submenu"`:

```tsx
{rows.map((row, i) => {
  if (row.kind === "item") {
    return (
      <div
        key={i}
        className={`context-menu-item${row.danger ? " danger" : ""}`}
        onClick={row.onClick}
      >
        {row.label}
      </div>
    );
  }
  if (row.kind === "submenu") {
    return (
      <div
        key={i}
        className="context-menu-item"
        onClick={row.onSubmenu}
        onMouseEnter={row.onSubmenu}
      >
        {row.label} ▸
      </div>
    );
  }
  return null;
})}
```

(Adapt to match the existing rendering pattern — the existing loop may use slightly different JSX.)

### Step 5: Verify

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

### Step 6: Commit

```
git -C /home/deez/farder add client/src/components/MemberContextMenu.tsx
git -C /home/deez/farder commit -m "feat(client): MemberContextMenu — Set language… submenu"
```

---

## Phase 5: Polish + smoke + CHANGELOG

## Task 7: Smoke + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

### Step 1: Run final build checks

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Both should be clean.

### Step 2: Manual smoke checklist (hand off to human)

- Open Settings → Translation: verify the default-target dropdown still works.
- Right-click a member with a foreign-language message → "Set language…" → pick a language → next message from that user translates without detection.
- Clear the override → next message goes through detection again.
- Send a message in your own language → "Already in <lang>".
- Send a short ambiguous message → low-confidence picker dropdown appears (not the old prompt).
- "Add a new language…" in the picker → download dialog → completes → new language selected.

### Step 3: CHANGELOG entry

Add under `### Added` in `CHANGELOG.md`, immediately after the v1.0 translation entry:

```markdown
- (2026-05-23) Message translation v1.1: replaces the `prompt()`-based low-confidence language picker with a real dropdown listing installed source languages + an "Add a new language…" entry that opens the download dialog inline. New right-click-member entry "Set language…" stores a per-user language override (keyed by public-key hex) in `~/.farder/settings.json` — once set, that user's messages skip auto-detection and route directly to the chosen source language. Same right-click submenu has a "Clear override" entry. Useful when franc keeps miscalling someone's messages or for users you know always write in language X. New: `SourceLanguagePicker.tsx` shared component. Modified: `translation.rs` (`TranslationSettings.user_language_overrides`), `types.ts`, `store.ts` (override-first lookup in `translateMessage`), `Message.tsx`, `TranslatedRow.tsx`, `MemberContextMenu.tsx`. No protocol or server changes.
```

### Step 4: Commit

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog entry for translation v1.1"
```

---

## Self-review notes

- All seven spec sections map to tasks: storage extension (Task 1), store override lookup (Task 2-3), shared picker (Task 4), TranslatedRow rewire (Task 5), MemberContextMenu submenu (Task 6), CHANGELOG (Task 7).
- The `authorPublicKeyHex` field is optional in TS to keep build clean across commits; runtime guard treats empty string as "no override lookup".
- Task 4's `SourceLanguagePicker` uses a window.prompt for "Add new language" as a pragmatic v1.1 scope — a full available-pairs picker would justify another spec.
- No Rust unit tests in this plan because the storage layer just reads/writes JSON; the existing `pick_entry_*` tests still pass.
