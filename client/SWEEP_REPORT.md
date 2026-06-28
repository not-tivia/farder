# Scale-Aware Click Positioning Sweep Report

## The Bug

On Windows at 125% DPI scaling (or any scale != 100%), `MouseEvent.clientX/Y` and
`getBoundingClientRect()` return **screen pixels**, but CSS `top`/`left` values on
`position: fixed` elements are interpreted as **CSS pixels** and then multiplied back
up by the scale factor.  Setting `left: clickX` therefore overshoots by the scale
factor and the popup flies off-screen.

Fix: measure `scale = rect.width / offsetWidth` from the live element, divide all
raw screen-px coordinates by `scale` before applying as CSS.

---

## Shared Helper API

**File:** `client/src/lib/useClickAnchoredPosition.ts`

```ts
function useClickAnchoredPosition(
  ref: RefObject<HTMLElement | null>,
  click: { x: number; y: number },
  opts?: {
    anchor?: "auto" | "toward-center"; // default "auto"
    capHeight?: boolean;               // default false
    elementW?: number;                 // fallback CSS width, default 180
    elementH?: number;                 // fallback CSS height, default 0
  }
): React.CSSProperties
```

- **`anchor: "auto"`** — opens rightward unless it would overflow the right edge, then opens leftward.
- **`anchor: "toward-center"`** — right-half-of-screen clicks open leftward (suitable for member-list/sidebar menus that should open toward the chat).
- **`capHeight: true`** — adds `maxHeight` + `overflowY: "auto"` so the element stays inside the viewport vertically.
- Internally measures `scale = rect.width / offsetWidth` via `useLayoutEffect` (runs after every render, stays current if zoom changes).
- All screen-px coordinates are divided by `scale` before being applied as CSS `top`/`left`/`bottom`.
- Bottom-half-of-screen clicks flip to bottom-anchor (element grows upward), matching the original UserProfilePopup behavior.

---

## Files Converted

| File | What changed |
|------|-------------|
| `client/src/lib/useClickAnchoredPosition.ts` | **NEW** — shared hook |
| `client/src/components/UserProfilePopup.tsx` | Removed manual scale + clamp block; replaced with `useClickAnchoredPosition(cardRef, position, { anchor: "toward-center", capHeight: true, elementW: 300 })` |
| `client/src/components/MemberContextMenu.tsx` | Removed manual `[scale, setScale]` + `useLayoutEffect` + manual clamp; replaced with `useClickAnchoredPosition(ref, position, { anchor: "toward-center" })` |
| `client/src/components/VoiceParticipantContextMenu.tsx` | Added `useClickAnchoredPosition` — previously used raw `top: position.y, left: position.x` with no scale compensation |
| `client/src/components/EmojiAutocomplete.tsx` | Added inline scale measurement (`useLayoutEffect`) and divides `position.y / scale`, `position.x / scale`. Note: this overlay is anchored to the textarea wrapper element rect (not a click), opens upward via `transform: translateY(-100%)` — `useClickAnchoredPosition` wasn't a fit; used the same core scale-measurement pattern directly |
| `client/src/components/Message.tsx` | Added `useClickAnchoredPosition` for both context menus: message right-click menu and image attachment context menu |
| `client/src/components/ChannelSidebar.tsx` | Added `useClickAnchoredPosition` for the channel/category right-click context menu |
| `client/src/components/ColorPickerPopover.tsx` | Added scale measurement; divides `anchorRect.bottom / scale` and `anchorRect.left / scale`. Note: anchored to element rect (from `getBoundingClientRect()`), not a click — same underlying bug since `getBoundingClientRect()` returns screen px |

---

## Intentionally Left Alone

| File | Reason |
|------|--------|
| `BanConfirmDialog.tsx` | `position: fixed` + `inset: 0` flex-centered modal — no click coordinates |
| `TranslationFirstRunModal.tsx` | Centered modal |
| `TranslationDownloadDialog.tsx` | Centered modal |
| `TimeoutDialog.tsx` | Centered modal |
| `KickedBannedDialog.tsx` | Centered modal |
| `CustomizeModal.tsx` | Centered modal |
| `BookBrowser.tsx`, `BookIntro.tsx`, `BookItemDetail.tsx` | `position: fixed` for full-screen overlays, not click-anchored |
| `GifSearchOptIn.tsx` | Centered modal |
| `CustomizerIntro.tsx` | Centered modal |
| `ScreenSharePopout.tsx` | Anchored via `getBoundingClientRect` of a specific trigger element with its own correct logic |
| `MessageSearchOverlay.tsx` | `position: fixed` centered — not click-anchored |
| `MediaPlayer.tsx` | No click-anchored fixed popup |
| `MemberSidebar.tsx` | Right-click opens `MemberContextMenu` (already fixed) |

---

## TypeScript Result

```
node node_modules/typescript/bin/tsc --noEmit
(no output — zero errors)
```

---

**RUNTIME-UNVERIFIED — owner verifies menus appear on-screen at 125% scale.**

The fix follows the identical pattern that was battle-tested and confirmed working
in `UserProfilePopup.tsx` and `MemberContextMenu.tsx` before this sweep.
