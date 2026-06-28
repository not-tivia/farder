/**
 * useClickAnchoredPosition
 *
 * Returns `React.CSSProperties` for a `position: fixed` overlay that is
 * anchored to a raw click coordinate (screen pixels as reported by
 * MouseEvent.clientX / clientY).
 *
 * ## The display-scale problem
 * On Windows at 125% (or any DPI scale > 100%), the browser reports click
 * coordinates in *screen* pixels, but interprets CSS `top`/`left` values in
 * *CSS* pixels and then multiplies them back up by the scale factor.  Setting
 * `left: clickX` therefore overshoots by `scale` and the popup flies off-screen.
 *
 * Fix: measure the device-pixel ratio by comparing the element's
 * `getBoundingClientRect().width` (screen px) to its `offsetWidth` (CSS px),
 * then divide raw screen coordinates by that scale before applying them as CSS.
 *
 * ## Options
 * - `anchor`: "auto" (default) — open rightward unless the right side has < 8 px
 *   clearance, then open leftward.  "toward-center" — right-half-of-screen
 *   clicks open leftward (useful for member-list / sidebar menus).
 * - `capHeight`: when true, add `maxHeight` + `overflowY: "auto"` so the
 *   element never overflows the viewport vertically.
 * - `elementW`: expected CSS width of the overlay (used when the element hasn't
 *   mounted yet, e.g. first render). Defaults to 180.
 * - `elementH`: expected CSS height of the overlay. Defaults to 0 (no vertical
 *   clamp on first render). Supply the measured offsetHeight for accurate clamping.
 */

import { useState, useLayoutEffect, type RefObject, type CSSProperties } from "react";

export interface ClickAnchoredPositionOpts {
  /** Horizontal anchor strategy. Default: "auto" */
  anchor?: "auto" | "toward-center";
  /** Add maxHeight/overflowY caps so the element stays inside the viewport. */
  capHeight?: boolean;
  /** Fallback CSS width (px) to use before the element mounts. Default: 180. */
  elementW?: number;
  /** Fallback CSS height (px) to use before the element mounts. Default: 0. */
  elementH?: number;
}

/** Margin (screen px) to keep from any viewport edge. */
const M = 8;

/**
 * @param ref     Ref attached to the overlay element (used to measure scale).
 * @param click   Raw mouse-event coordinates: { x: clientX, y: clientY }.
 * @param opts    Optional tuning (see above).
 * @returns       CSSProperties to spread onto the overlay div.
 */
export function useClickAnchoredPosition(
  ref: RefObject<HTMLElement | null>,
  click: { x: number; y: number },
  opts: ClickAnchoredPositionOpts = {},
): CSSProperties {
  const {
    anchor = "auto",
    capHeight = false,
    elementW: fallbackW = 180,
    elementH: fallbackH = 0,
  } = opts;

  // Measure the CSS-px → screen-px scale from the live element.
  // getBoundingClientRect() returns screen px; offsetWidth returns CSS px.
  // Running after every render (no dep array) keeps it fresh if zoom changes.
  const [scale, setScale] = useState(1);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measured = el.getBoundingClientRect().width / (el.offsetWidth || 1);
    if (measured > 0.1 && Math.abs(measured - scale) > 0.01) setScale(measured);
  });

  const vw = document.documentElement.clientWidth || window.innerWidth;   // screen px
  const vh = document.documentElement.clientHeight || window.innerHeight; // screen px

  // Element dimensions in screen px (use measured if available, else fallback).
  const el = ref.current;
  const elCssW = el ? (el.offsetWidth || fallbackW) : fallbackW;
  const elCssH = el ? (el.offsetHeight || fallbackH) : fallbackH;
  const elScreenW = elCssW * scale;
  const elScreenH = elCssH * scale;

  // --- Horizontal ---
  let leftScreen: number;
  if (anchor === "toward-center") {
    // Right-half clicks open leftward (toward chat area); left-half open rightward.
    leftScreen = click.x > vw / 2 ? click.x - elScreenW : click.x;
  } else {
    // "auto": open rightward unless it would overflow, then open leftward.
    leftScreen = click.x + elScreenW + M > vw ? click.x - elScreenW : click.x;
  }
  // Clamp both edges.
  leftScreen = Math.max(M, Math.min(leftScreen, vw - elScreenW - M));

  // --- Vertical ---
  // Bottom-half clicks anchor the bottom edge so the element grows upward.
  const openUp = click.y > vh / 2;
  const topScreen = Math.max(M, click.y);
  const bottomScreen = Math.max(M, vh - click.y);

  // Clamp downward-opening menus too (if we know the height).
  const clampedTop = openUp ? topScreen : Math.min(topScreen, Math.max(M, vh - elScreenH - M));

  const pos: CSSProperties = {
    position: "fixed",
    left: leftScreen / scale,
    right: "auto",
    ...(openUp
      ? { bottom: bottomScreen / scale, top: "auto" }
      : { top: clampedTop / scale, bottom: "auto" }),
  };

  if (capHeight) {
    pos.maxHeight = `${(vh - 2 * M) / scale}px`;
    pos.overflowY = "auto";
  }

  return pos;
}
