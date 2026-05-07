# Bundled Twemoji Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace native Unicode emoji rendering with bundled Twemoji SVG images so every Farder client renders emoji identically across platforms.

**Architecture:** Vendor `@twemoji/svg` (~8 MB SVG set, jdecked's active fork) into `client/public/twemoji/svg/`. Modify the existing `renderUnicodeEmoji()` single-swap-point in `client/src/lib/unicodeEmoji.tsx` to return an `<img>` tag pointing at the static asset, with `onError` falling back to native rendering. Update `ReactionPicker` to use the same helper for visual consistency.

**Tech Stack:** TypeScript + React. No protocol or backend changes.

**Spec:** `docs/superpowers/specs/2026-05-06-bundled-twemoji-design.md`

---

## File structure

**New (vendored, ~3,200 files):**
- `client/public/twemoji/svg/<hex>.svg`

**Modified:**
- `client/src/lib/unicodeEmoji.tsx` — add `codepointToTwemojiName` helper, change `renderUnicodeEmoji` to render `<img>` with onError fallback
- `client/src/components/ReactionPicker.tsx` — wrap codepoints in `renderUnicodeEmoji`
- `.gitattributes` — mark SVG assets as binary (no text diffs)
- `CHANGELOG.md` — entry

---

## Task 1: Vendor the Twemoji SVG assets

One-time install + copy + uninstall. Result is `client/public/twemoji/svg/*.svg` committed verbatim, no runtime dependency.

**Files:**
- Create: `client/public/twemoji/svg/*.svg` (~3,200 files)
- Modify: `.gitattributes`

- [ ] **Step 1: Install the package as a dev-time helper**

```
cd /home/deez/farder/client && npm install --save-dev @twemoji/svg@15.0.0
```

Expected: package added to `devDependencies`. The package contains `node_modules/@twemoji/svg/<hex>.svg` files at the root of the package (~3,200 files, ~8 MB).

- [ ] **Step 2: Verify the package layout**

```
ls /home/deez/farder/client/node_modules/@twemoji/svg/ | head -5
ls /home/deez/farder/client/node_modules/@twemoji/svg/*.svg | wc -l
```

Expected: at least 3,000 `.svg` files. If the package layout differs (subdir like `svg/` or `assets/`), adjust the copy command in Step 4 accordingly. List the directory tree first with `find /home/deez/farder/client/node_modules/@twemoji/svg -maxdepth 2 -type d` if uncertain.

- [ ] **Step 3: Create the destination directory**

```
mkdir -p /home/deez/farder/client/public/twemoji/svg
```

- [ ] **Step 4: Copy all SVGs to the public folder**

```
cp /home/deez/farder/client/node_modules/@twemoji/svg/*.svg /home/deez/farder/client/public/twemoji/svg/
ls /home/deez/farder/client/public/twemoji/svg/ | wc -l
```

Expected: same count as Step 2 (~3,000+ files).

- [ ] **Step 5: Sanity-check a few common emoji**

```
ls /home/deez/farder/client/public/twemoji/svg/1f604.svg /home/deez/farder/client/public/twemoji/svg/1f44d.svg /home/deez/farder/client/public/twemoji/svg/2764.svg
```

Expected: all three files exist (😄 grinning, 👍 thumbs up, ❤ heart).

- [ ] **Step 6: Uninstall the package**

```
cd /home/deez/farder/client && npm uninstall @twemoji/svg
```

Expected: `@twemoji/svg` removed from `devDependencies`. The vendored SVGs in `public/twemoji/svg/` are unaffected — they were copied, not symlinked.

- [ ] **Step 7: Mark SVGs as binary in `.gitattributes`**

Append to `/home/deez/farder/.gitattributes` (create if missing):

```
client/public/twemoji/svg/*.svg binary
```

This stops git from trying to compute textual diffs on the asset files.

- [ ] **Step 8: Commit the vendored assets**

```
git -C /home/deez/farder add client/public/twemoji/svg/ .gitattributes client/package.json client/package-lock.json
git -C /home/deez/farder commit -m "feat(client): vendor @twemoji/svg 15.0.0 SVG assets"
```

The commit will be large (~3,200 new files, ~8 MB) — that's expected and one-time.

- [ ] **Step 9: Verify dev server still serves the SVGs**

If `npm run tauri dev` is running, refresh the window and run in the browser devtools:
```js
fetch('/twemoji/svg/1f604.svg').then(r => r.status)
```

Expected: 200. If 404, restart `npm run tauri dev` (Vite needs to rescan `public/`).

---

## Task 2: Implement `codepointToTwemojiName` + swap `renderUnicodeEmoji`

The single swap. Adds the helper that converts a Unicode emoji string to its Twemoji filename, then changes `renderUnicodeEmoji` to render an `<img>` with onError fallback to native.

**Files:**
- Modify: `client/src/lib/unicodeEmoji.tsx`

- [ ] **Step 1: Add the helper + update renderUnicodeEmoji**

Replace the current `renderUnicodeEmoji` function (the last function in the file, around line 91-97) with:

```tsx
/**
 * Convert a Unicode emoji string to a Twemoji filename (without `.svg` suffix).
 *
 * Rules (matching Twemoji's own naming convention):
 * - Each codepoint becomes its lowercase hex (e.g. U+1F604 → "1f604").
 * - Multi-codepoint sequences (ZWJ, regional indicators, skin-tone modifiers)
 *   join codepoints with "-".
 * - The variation-selector-16 (U+FE0F) is stripped, since Twemoji's filenames
 *   omit it (e.g. ❤️ "U+2764 U+FE0F" → "2764", NOT "2764-fe0f").
 *
 * Examples:
 *   "😄" → "1f604"
 *   "❤️" → "2764"
 *   "🇺🇸" → "1f1fa-1f1f8"
 *   "👍🏽" → "1f44d-1f3fd"
 *   "👨‍👩‍👧" → "1f468-200d-1f469-200d-1f467"
 */
export function codepointToTwemojiName(codepoint: string): string {
  const parts: string[] = [];
  for (const char of codepoint) {
    const cp = char.codePointAt(0);
    if (cp === undefined) continue;
    if (cp === 0xfe0f) continue; // strip variation selector 16
    parts.push(cp.toString(16));
  }
  return parts.join("-");
}

/**
 * Render a Unicode emoji codepoint as a React node.
 *
 * Renders as a Twemoji SVG `<img>` for cross-platform consistency.
 * Falls back to native rendering (a `<span>` with the codepoint) if the
 * Twemoji asset is missing — handled inline via onError.
 */
export function renderUnicodeEmoji(codepoint: string): ReactNode {
  const name = codepointToTwemojiName(codepoint);
  return (
    <img
      src={`/twemoji/svg/${name}.svg`}
      alt={codepoint}
      className="unicode-emoji twemoji"
      draggable={false}
      style={{
        height: "1.2em",
        width: "1.2em",
        verticalAlign: "-0.2em",
        userSelect: "none",
        display: "inline-block",
      }}
      onError={(e) => {
        // Twemoji asset missing — replace with a native-rendering span.
        const img = e.currentTarget as HTMLImageElement;
        const span = document.createElement("span");
        span.className = "unicode-emoji";
        span.textContent = codepoint;
        img.replaceWith(span);
      }}
    />
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Hot-reload check**

If `npm run tauri dev` is running, switch to the window and verify a recent message containing emoji now shows Twemoji glyphs (yellow Twitter-style faces). If the dev server isn't running, skip this step — Task 5's smoke covers it.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/lib/unicodeEmoji.tsx
git -C /home/deez/farder commit -m "feat(client): renderUnicodeEmoji uses bundled Twemoji SVGs with native fallback"
```

---

## Task 3: Update ReactionPicker to use renderUnicodeEmoji

The picker currently renders `COMMON_EMOJI` codepoints directly. Wrap each in `renderUnicodeEmoji` so the picker's strip matches the message rendering visually.

**Files:**
- Modify: `client/src/components/ReactionPicker.tsx`

- [ ] **Step 1: Add the import**

At the top of `client/src/components/ReactionPicker.tsx`, alongside existing imports:

```tsx
import { renderUnicodeEmoji } from "../lib/unicodeEmoji";
```

- [ ] **Step 2: Replace the codepoint render in the button**

Find the section that maps `COMMON_EMOJI` to `<button>` elements (around line 93-101 — the relevant snippet looks like):

```tsx
{COMMON_EMOJI.map((emoji) => (
  <button
    key={emoji}
    ...
    onClick={() => onSelect(emoji)}
    title={emoji}
  >
    {emoji}
  </button>
))}
```

Replace the inner `{emoji}` with `{renderUnicodeEmoji(emoji)}`:

```tsx
{COMMON_EMOJI.map((emoji) => (
  <button
    key={emoji}
    ...
    onClick={() => onSelect(emoji)}
    title={emoji}
  >
    {renderUnicodeEmoji(emoji)}
  </button>
))}
```

The `onClick={() => onSelect(emoji)}` is unchanged — selection still works on the codepoint string. The `title={emoji}` keeps the codepoint as the tooltip so users can confirm what they're clicking.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/ReactionPicker.tsx
git -C /home/deez/farder commit -m "feat(client): ReactionPicker uses renderUnicodeEmoji for Twemoji consistency"
```

---

## Task 4: Reaction count chips also pick up Twemoji rendering

Reactions on messages render the emoji glyph in a count chip (e.g. "👍 3"). Verify those go through `renderUnicodeEmoji` too. If they don't (current code likely renders the codepoint string directly), update them.

**Files:**
- Modify (potentially): `client/src/components/Message.tsx` or `client/src/components/RenderedMessageContent.tsx`

- [ ] **Step 1: Find where reaction emoji are rendered in chips**

```
grep -n "reaction\|reactions\.map\|emoji" /home/deez/farder/client/src/components/Message.tsx | head -20
```

Look for a place where a `reaction.emoji` (string) is rendered inside a chip-style element. A typical pattern:

```tsx
{message.reactions.map((r) => (
  <button key={r.emoji} className="reaction-chip" ...>
    {r.emoji} {r.count}
  </button>
))}
```

The custom-emoji case (where `r.file_id != null`) renders an image — leave it alone. Only the unicode case renders the raw codepoint.

- [ ] **Step 2: Wrap the unicode-emoji branch in renderUnicodeEmoji**

If the codepoint is rendered directly, change the chip's content to:

```tsx
{r.file_id != null
  ? <img src={...} alt={r.emoji} className="reaction-chip-img" />
  : renderUnicodeEmoji(r.emoji)
} {r.count}
```

Add the import at the top of the file if missing:
```tsx
import { renderUnicodeEmoji } from "../lib/unicodeEmoji";
```

If the file already imports `renderUnicodeEmoji` (because RenderedMessageContent does), just re-use it.

If the existing code is structured differently (e.g. the unicode/custom branch is inside a child component), apply the change there. Don't refactor for refactor's sake — minimum-diff replacement.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit (if any change was made)**

```
git -C /home/deez/farder add client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): reaction count chips use renderUnicodeEmoji"
```

If Step 1 found that reaction chips already go through `renderUnicodeEmoji` (or some equivalent), skip the commit and note in the smoke test that they already render correctly.

---

## Task 5: Smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart dev session and walk smoke tests**

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Confirm in Alice's window:

- [ ] Existing message with `:smile:` (😄) renders as a yellow Twitter-style smiley, not the native OS emoji.
- [ ] Send a flag (🇺🇸) — renders as a Twemoji flag.
- [ ] Send a ZWJ family (👨‍👩‍👧) — renders as a single Twemoji glyph (not three separate emoji).
- [ ] Send a skin-toned emoji (👍🏽) — renders with the correct skin tone.
- [ ] Send a heart (❤️) — renders as Twemoji heart (filename is `2764.svg`, with FE0F stripped).
- [ ] Open the ReactionPicker (👍 button on a message) — emoji strip shows Twemoji glyphs.
- [ ] React with an emoji → the count chip on the message shows the Twemoji glyph.
- [ ] Browser devtools network tab: confirm `/twemoji/svg/<hex>.svg` requests return 200 (served from local public/).
- [ ] Test fallback: in devtools, throttle network to "Offline" briefly and send a brand-new emoji whose Twemoji file you've temporarily renamed (e.g. `mv public/twemoji/svg/1f604.svg /tmp/1f604.svg.bak`, send 😄, observe native fallback, then `mv` it back). The message should still render — just with native OS emoji for that one glyph.

If any item fails, file a follow-up — don't fix in this commit.

- [ ] **Step 2: Add CHANGELOG entry**

In `CHANGELOG.md`, under the most recent `### Added` block:

```
- (2026-05-06) Bundled Twemoji emoji rendering. Native OS emoji are now replaced with Twemoji SVG glyphs (Twitter-style) so every Farder client renders emoji identically — Linux, Windows, Mac, anywhere a WebView runs. Assets vendored from @twemoji/svg 15.0.0 (jdecked's active fork) into `client/public/twemoji/svg/` (~3,200 SVG files, ~8 MB, one-time). The previously-prepared `renderUnicodeEmoji()` single-swap-point in `client/src/lib/unicodeEmoji.tsx` swaps from `<span>` to `<img src="/twemoji/svg/<hex>.svg">` with an `onError` fallback to native rendering for any emoji not in the Twemoji set. Reaction picker and reaction count chips also pick up the new rendering for full consistency. No protocol or persistence changes — emoji on the wire are still Unicode codepoints; only the rendering swaps. Copy-paste preserves the original codepoint via the image's `alt` attribute.
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for bundled Twemoji"
```

---

## Self-review notes

**Spec coverage:**
- `renderUnicodeEmoji` swap (single point) → Task 2
- `codepointToTwemojiName` helper with FE0F stripping + ZWJ joining → Task 2
- onError fallback to native rendering → Task 2
- Vendored SVGs in `client/public/twemoji/svg/` → Task 1
- `ReactionPicker` consistency → Task 3
- Reaction count chips → Task 4
- `.gitattributes` marking SVG as binary → Task 1 Step 7
- No protocol changes → no protocol work in any task
- Copy-paste preserves codepoint → covered by `alt={codepoint}` in Task 2's `<img>` element
- Accessibility — screen readers announce alt → same alt
- Smoke covering ZWJ, flags, skin tones, fallback, heart's FE0F edge case → Task 5

**Type/name consistency:**
- `codepointToTwemojiName(codepoint: string): string` — defined and used in Task 2 only.
- `renderUnicodeEmoji(codepoint: string): ReactNode` — signature unchanged from current code; callers in `RenderedMessageContent.tsx` and `EmojiAutocomplete.tsx` need no edits.
- `<img>` className `unicode-emoji twemoji` matches the spec.

**Known compromises:**
- Twemoji set may lag the latest Unicode release (e.g. Unicode 16 emojis added after the 15.0.0 package release won't have files). The `onError` fallback handles this gracefully.
- The CSS sizing is inline in the React component (height/width/vertical-align). If a theme ever wants per-theme emoji sizing, it can override via `.twemoji` selector with `!important`. This is intentional — keeping it inline avoids editing all three theme CSS files for a base rendering rule.
- `.gitattributes` binary marker doesn't shrink the commit; it just prevents pointless diffs. The 8 MB asset cost is accepted per the design.
- No automated tests for `codepointToTwemojiName`. The function is small and the smoke test in Task 5 covers all the tricky cases (ZWJ, flags, skin tones, FE0F-stripping via the heart test). If you want a sanity check, paste the function into a Node REPL with the example inputs from its docstring.

---

## Execution

Plan complete and saved to `docs/superpowers/plans/2026-05-06-bundled-twemoji.md`.

Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks. Five small tasks; total run is short.

**2. Inline Execution** — execute in this session with checkpoints.

Subagent-Driven is the better fit here since Task 1 (the asset vendoring) is mechanical and isolated, Task 4 has a "find the relevant code first" exploration step that benefits from a fresh subagent's focus, and Task 5 is human-driven smoke testing.
