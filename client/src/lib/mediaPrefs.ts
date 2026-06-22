// Remembered <video> volume. Defaults to 15%; fails safe to 0.15 on any error.
const KEY = "farder.mediaVolume";
const DEFAULT = 0.15;

/**
 * Read the remembered volume (0..1), or 0.15.
 * Test-notes (verified by inspection):
 *   - nothing saved      → 0.15
 *   - saved "0.4"        → 0.4
 *   - saved "5" (out of range) → 0.15
 *   - saved "abc" / throws → 0.15
 */
export function getMediaVolume(): number {
  try {
    const v = parseFloat(localStorage.getItem(KEY) ?? "");
    return v >= 0 && v <= 1 ? v : DEFAULT;
  } catch { return DEFAULT; }
}

/** Persist the volume (0..1); ignores out-of-range + storage errors. */
export function setMediaVolume(v: number): void {
  try { if (v >= 0 && v <= 1) localStorage.setItem(KEY, String(v)); } catch { /* ignore */ }
}
