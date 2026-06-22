// Remembered floating-player placement, persisted client-side. Default anchors
// to the right of the chat column so a floating player doesn't cover messages.
// Fails safe (returns the default) on any storage error.

export interface FloatAnchor { x: number; y: number; w: number; h: number }

const KEY = "farder.floatAnchor";
const SIZE = { w: 360, h: 240 };

/** Default = upper-right area of the viewport (right of the chat column). */
function defaultAnchor(): FloatAnchor {
  const vw = typeof window !== "undefined" ? window.innerWidth : 1280;
  return { x: Math.max(16, vw - SIZE.w - 32), y: 88, w: SIZE.w, h: SIZE.h };
}

/**
 * Read the saved anchor, or the right-of-chat default.
 * Test-notes (verified by inspection):
 *   - nothing saved            → defaultAnchor() (x near right edge, y 88)
 *   - saved {x,y,w,h} valid    → that object
 *   - saved malformed/partial  → defaultAnchor() (validation rejects it)
 *   - localStorage throws       → defaultAnchor() (caught)
 */
export function getFloatAnchor(): FloatAnchor {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return defaultAnchor();
    const a = JSON.parse(raw) as Partial<FloatAnchor>;
    if (
      typeof a.x === "number" && typeof a.y === "number" &&
      typeof a.w === "number" && typeof a.h === "number" &&
      a.w > 80 && a.h > 60
    ) return { x: a.x, y: a.y, w: a.w, h: a.h };
    return defaultAnchor();
  } catch { return defaultAnchor(); }
}

/** Persist the anchor; swallows storage errors. */
export function setFloatAnchor(a: FloatAnchor): void {
  try { localStorage.setItem(KEY, JSON.stringify(a)); } catch { /* ignore */ }
}

const ALWAYS_KEY = "farder.alwaysFloat";
/** Whether ▶ should open players directly floating. Default false; fail-safe false. */
export function getAlwaysFloat(): boolean {
  try { return localStorage.getItem(ALWAYS_KEY) === "1"; } catch { return false; }
}
export function setAlwaysFloat(v: boolean): void {
  try { if (v) localStorage.setItem(ALWAYS_KEY, "1"); else localStorage.removeItem(ALWAYS_KEY); } catch { /* ignore */ }
}
