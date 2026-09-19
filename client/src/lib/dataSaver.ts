// Data Saver settings: a single localStorage-backed store, read through
// DataSaverContext. Client-only; fails safe to defaults on any error.
const KEY = "farder.dataSaver";

export interface DataSaverSettings {
  enabled: boolean;            // master switch
  gateImages: boolean;         // images over threshold -> click-to-load
  clickToLoadEmbeds: boolean;  // link previews -> "Load preview"
  freezeAvatars: boolean;      // animated avatars -> still first frame
  thresholdMB: number;         // size cutoff for images, in MB
  /** Hard ceiling, in MB, above which NOTHING downloads without being asked —
   *  images included, Data Saver on or off. Separate from `thresholdMB` on
   *  purpose: that one is a preference about saving data, this one is about not
   *  letting someone else's 2 GB clip start arriving on your connection because
   *  you scrolled past it. */
  askAboveMB: number;
}

export const DATA_SAVER_DEFAULTS: DataSaverSettings = {
  enabled: false,
  gateImages: true,
  clickToLoadEmbeds: true,
  freezeAvatars: true,
  thresholdMB: 1,
  askAboveMB: 8,
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

export function askAboveBytes(s: DataSaverSettings): number {
  // 0 or a negative value would mean "ask about everything", which is a
  // legitimate choice; NaN from a hand-edited store is not, and defaults.
  const mb = Number.isFinite(s.askAboveMB) ? Math.max(0, s.askAboveMB) : DATA_SAVER_DEFAULTS.askAboveMB;
  return mb * 1024 * 1024;
}

/**
 * True when a file is large enough that it must not be fetched until the person
 * receiving it says so.
 *
 * Unconditional by design — it does not consult `enabled`. Data Saver is a
 * preference someone opts into; this is the floor under everyone, because the
 * cost of getting it wrong is paid by the person on the metered connection who
 * did nothing but open a channel.
 *
 * Test-notes (verified by inspection), ceiling 8 MB:
 *   - 2 MB, Data Saver off -> false
 *   - 40 MB, Data Saver off -> true
 *   - 40 MB, Data Saver on  -> true
 */
export function needsDownloadConsent(s: DataSaverSettings, sizeBytes: number): boolean {
  return sizeBytes > askAboveBytes(s);
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
