# Bundled Twemoji — Design Spec

**Date:** 2026-05-06

## Goal

Replace native OS Unicode emoji rendering with bundled Twemoji SVG images so that every Farder client renders emoji identically — Linux, Windows, Mac, mobile-via-WebView all see the same glyph. End-game UX positioning: "your emoji look the same as your friends', regardless of platform."

## Non-goals

- Custom server-side emoji uploads (already shipped as Reaction Book).
- Animated emoji (Twemoji is static).
- Skin-tone modifier picker UI (renders correctly via existing codepoint joining; no new picker affordance).
- Replacing the system emoji picker for input. Users still type emoji via their OS picker or `:shortcode:` autocomplete; this spec is only about rendering them.

## Architecture

`client/src/lib/unicodeEmoji.tsx` already exposes a `renderUnicodeEmoji(codepoint)` function as the single swap point — called from `RenderedMessageContent.tsx` and `EmojiAutocomplete.tsx`. Today it returns `<span>{codepoint}</span>` (native). The spec changes its body to `<img src={twemojiUrl(codepoint)} onError={fallback}>`, with native rendering as the onError fallback.

Twemoji SVG assets are vendored into `client/public/twemoji/svg/` and served as static files by Vite (dev) and the bundled `dist/` (release). One-time download at design time; not a runtime fetch.

`ReactionPicker.tsx` is updated to use the same `renderUnicodeEmoji` helper for its `COMMON_EMOJI` strip so the picker matches the message rendering.

## Components

### `client/src/lib/unicodeEmoji.tsx` (modified)

- Add helper `codepointToTwemojiName(codepoint: string): string` that converts a Unicode emoji string (potentially multi-codepoint with ZWJ joiners) into the Twemoji filename convention: lowercase hex codepoints joined by `-`, with the variation-selector-16 (`U+FE0F`) stripped per Twemoji's own convention.
- Modify `renderUnicodeEmoji(codepoint: string)` to return:

```tsx
<img
  src={`/twemoji/svg/${codepointToTwemojiName(codepoint)}.svg`}
  alt={codepoint}
  className="unicode-emoji twemoji"
  draggable={false}
  onError={(e) => {
    // Fall back to native rendering if Twemoji asset missing.
    const span = document.createElement("span");
    span.className = "unicode-emoji";
    span.textContent = codepoint;
    (e.currentTarget as HTMLImageElement).replaceWith(span);
  }}
/>
```

- The `alt` carries the original codepoint so screen readers and copy-paste work.
- The `unicode-emoji twemoji` class lets themes style the image (default size, vertical alignment, etc).

### `client/public/twemoji/svg/` (new)

Vendored SVG assets from a Twemoji distribution. Acquisition is documented in the implementation plan: download from `https://github.com/jdecked/twemoji` (active community fork) or `npm i -D @twemoji/svg` then copy `node_modules/@twemoji/svg/**/*.svg` into `client/public/twemoji/svg/`. ~3,200 files, ~5–7 MB total.

A `.gitattributes` rule marks `*.svg` under `client/public/twemoji/` as binary so git doesn't try to diff them.

### `client/src/components/ReactionPicker.tsx` (modified)

The `COMMON_EMOJI` strip currently renders raw codepoints inside `<button>` children. Wrap each codepoint in `renderUnicodeEmoji(emoji)` so the picker matches the message rendering. The button still calls `onSelect(emoji)` with the codepoint string (no change to selection behavior).

### `client/src/index.css` (or whichever file holds existing emoji styles) (modified)

Add a CSS rule for `.twemoji`:

```css
.twemoji {
  display: inline-block;
  height: 1.2em;
  width: 1.2em;
  vertical-align: -0.2em;
  user-select: none;
}
```

Sizes the image to match surrounding text, with a slight downward shift to align with the baseline like a native emoji would.

## Data flow

No protocol or persistence changes. Emoji are stored on the wire as Unicode codepoints exactly as before. The only difference is rendering at the receiving client: native `<span>` becomes `<img src="/twemoji/...">`.

## Edge cases

| Case | Handling |
|---|---|
| Unknown emoji (Twemoji asset missing) | `onError` swaps the `<img>` for a `<span>{codepoint}</span>` — native render for that one emoji. |
| ZWJ sequence (e.g. 👨‍👩‍👧 family) | `codepointToTwemojiName` joins all non-FE0F codepoints with `-`. Twemoji ships these as single files. |
| Variation selector 16 (`U+FE0F`) | Stripped before joining — matches Twemoji's filename convention. |
| Skin-tone modifier (e.g. 👍🏽) | Multi-codepoint, joined with `-`. Twemoji has a dedicated file per skin-toned variant. |
| Flag (regional indicator pair, e.g. 🇺🇸) | Two codepoints joined with `-`, e.g. `1f1fa-1f1f8.svg`. |
| Copy-paste from a Farder message | Browser/Tauri serializes the `<img>` `alt` attribute (the original codepoint), so the user copies the codepoint, not the image. |
| Accessibility / screen readers | `alt={codepoint}` so Unicode emoji are announced as before. |
| Reaction count rendering | Reactions store the emoji as a string; counts and "me" indicators are unchanged. The emoji glyph in the count chip just renders via Twemoji like everywhere else. |

## Testing

- **Manual smoke** (in the implementation plan):
  - Type `:smile:` → message shows the Twemoji 😄 (yellow Twitter style).
  - Send a flag 🇺🇸, a ZWJ family 👨‍👩‍👧, a skin-toned 👍🏽 — all render correctly.
  - Open ReactionPicker — strip shows Twemoji glyphs.
  - Type a brand-new emoji not in Twemoji's set (or temporarily rename a file) → fallback to native renders for that one item, page doesn't break.
  - Verify identical look on Linux, Windows, Mac if possible.
- **No automated tests** — the rendering swap is mechanical and visually verifiable.

## Implementation files

**Modified:**
- `client/src/lib/unicodeEmoji.tsx`
- `client/src/components/ReactionPicker.tsx`
- `client/src/index.css` (or theme CSS — wherever the base styles live)
- `.gitattributes`

**New:**
- `client/public/twemoji/svg/*.svg` (vendored; ~3,200 files, ~5–7 MB)

**Modified (docs):**
- `CHANGELOG.md`

## Acquisition note

The implementation plan will document the exact download command (likely `npm install --save-dev @twemoji/svg && cp -r node_modules/@twemoji/svg/* client/public/twemoji/svg/ && npm uninstall @twemoji/svg`). The package is then removed from `package.json` so the assets are committed verbatim and not re-downloaded on every install.

## Backwards compatibility

No protocol changes. Old clients still see native rendering; new clients see Twemoji. Mixed sessions look slightly different per-user but functionally identical.

## Acceptance criteria

- All Unicode emoji in messages, autocomplete previews, and the reaction picker render as Twemoji SVGs.
- Missing Twemoji assets fall back to native rendering without breaking the page.
- Copy-paste of a message preserves the original Unicode codepoints.
- Theme CSS variables still apply (size, alignment) via the `.twemoji` class.
- ~5–7 MB added to the repo for the vendored assets (one-time cost, no runtime network).
