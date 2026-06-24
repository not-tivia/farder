// Data Saver settings: a single localStorage-backed store, read through
// DataSaverContext. Client-only; fails safe to defaults on any error.
const KEY = "farder.dataSaver";

export interface DataSaverSettings {
  enabled: boolean;            // master switch
  gateImages: boolean;         // images over threshold -> click-to-load
  clickToLoadEmbeds: boolean;  // link previews -> "Load preview"
  freezeAvatars: boolean;      // animated avatars -> still first frame
  thresholdMB: number;         // size cutoff for images, in MB
}

export const DATA_SAVER_DEFAULTS: DataSaverSettings = {
  enabled: false,
  gateImages: true,
  clickToLoadEmbeds: true,
  freezeAvatars: true,
  thresholdMB: 1,
};

/**
 * Read settings, filling any missing keys from defaults.
 * Test-notes (verified by inspection):
 *   - nothing saved          -> DATA_SAVER_DEFAULTS
 *   - partial {enabled:true} -> defaults merged, enabled:true
 *   - invalid JSON / throws  -> DATA_SAVER_DEFAULTS
 */
export function getDataSaver(): DataSaverSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DATA_SAVER_DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<DataSaverSettings>;
    return { ...DATA_SAVER_DEFAULTS, ...parsed };
  } catch {
    return { ...DATA_SAVER_DEFAULTS };
  }
}

/** Persist settings; ignores storage errors. */
export function setDataSaver(s: DataSaverSettings): void {
  try { localStorage.setItem(KEY, JSON.stringify(s)); } catch { /* ignore */ }
}

/** True once anything has been saved (gates the one-time migration). */
export function hasDataSaver(): boolean {
  try { return localStorage.getItem(KEY) != null; } catch { return false; }
}

export function thresholdBytes(s: DataSaverSettings): number {
  return Math.max(0, s.thresholdMB) * 1024 * 1024;
}

/**
 * True when an image of sizeBytes should be held behind a click-to-load gate.
 * Test-notes (verified by inspection), threshold 1 MB:
 *   - disabled                 -> false
 *   - enabled, gateImages off  -> false
 *   - enabled, 500 KB          -> false
 *   - enabled, 4 MB            -> true
 */
export function imageIsGated(s: DataSaverSettings, sizeBytes: number): boolean {
  return s.enabled && s.gateImages && sizeBytes > thresholdBytes(s);
}

/**
 * True for animated-image data URLs we should freeze.
 * Test-notes (verified by inspection):
 *   - "data:image/gif;base64,..."  -> true
 *   - "data:image/webp;base64,..." -> true
 *   - "data:image/png;base64,..."  -> false
 *   - null/undefined               -> false
 */
export function isAnimatedDataUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  return /^data:image\/(gif|apng|webp)/i.test(url);
}
