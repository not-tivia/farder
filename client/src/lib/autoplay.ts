// Autoplay: media from people YOU have chosen to trust starts by itself, muted,
// in chat and in the top-of-screen pop-up. Everything else keeps the existing
// click-to-play behaviour. Off by default.
//
// Stored on this device only (localStorage); fails closed to "off" on any error.
// The decision is a pure function so it can be tested without a DOM.

export interface AutoplaySettings {
  /** Master switch. Default OFF. */
  enabled: boolean;
  /** Also show trusted senders' media inside the notification pop-up. */
  inPopups: boolean;
  /** Public keys (`vk_<hex>`) of the people whose media may autoplay. */
  trusted: string[];
}

export const AUTOPLAY_DEFAULTS: AutoplaySettings = { enabled: false, inPopups: true, trusted: [] };

const KEY = "farder.autoplay";
export const AUTOPLAY_CHANGED = "farder:autoplay-changed";
/** More trusted people than this is not a short list any more. */
export const MAX_TRUSTED = 200;

export function getAutoplay(): AutoplaySettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...AUTOPLAY_DEFAULTS, trusted: [] };
    const v = JSON.parse(raw) as Partial<AutoplaySettings>;
    const trusted = Array.isArray(v.trusted)
      ? v.trusted.filter((x): x is string => typeof x === "string").slice(0, MAX_TRUSTED)
      : [];
    return { enabled: v.enabled === true, inPopups: v.inPopups !== false, trusted };
  } catch {
    return { ...AUTOPLAY_DEFAULTS, trusted: [] };
  }
}

export function setAutoplay(next: AutoplaySettings): void {
  const clean: AutoplaySettings = {
    enabled: next.enabled,
    inPopups: next.inPopups,
    trusted: Array.from(new Set(next.trusted)).slice(0, MAX_TRUSTED),
  };
  try { localStorage.setItem(KEY, JSON.stringify(clean)); } catch { /* storage unavailable */ }
  if (typeof window !== "undefined") window.dispatchEvent(new CustomEvent(AUTOPLAY_CHANGED));
}

export function setTrusted(s: AutoplaySettings, pk: string, trusted: boolean): AutoplaySettings {
  const rest = s.trusted.filter((x) => x !== pk);
  return { ...s, trusted: trusted ? [...rest, pk] : rest };
}

export interface AutoplayContext {
  /** Data Saver's master switch: while on, nothing autoplays. */
  dataSaverOn: boolean;
  /** The OS "reduce motion" preference: honoured, always. */
  reducedMotion: boolean;
}

/**
 * Whether media from `authorPk` may start by itself. Every condition must hold,
 * and an unknown author never qualifies.
 *
 * `where` separates the chat from the pop-up, which has its own switch because a
 * pop-up appears over whatever else is on screen.
 */
export function shouldAutoplay(
  s: AutoplaySettings,
  authorPk: string | null | undefined,
  ctx: AutoplayContext,
  where: "chat" | "popup" = "chat",
): boolean {
  if (!authorPk) return false;
  if (!s.enabled) return false;
  if (ctx.dataSaverOn || ctx.reducedMotion) return false;
  if (where === "popup" && !s.inPopups) return false;
  return s.trusted.includes(authorPk);
}

const MEDIA_EXT = /\.(?:mp4|webm|mov|m4v|gif|png|jpe?g|webp)(?:\?[^\s]*)?$/i;

/**
 * The first plain https link in `text` that points straight at a video or image
 * file -- the only thing a pop-up will preview. Pages (a tweet, a YouTube link)
 * are never previewed in a pop-up: they need a lookup and, for YouTube, consent.
 */
export function firstDirectMediaUrl(text: string): string | null {
  const urls = text.match(/https:\/\/[^\s<>"']+/gi) ?? [];
  for (const u of urls) {
    const clean = u.replace(/[),.;!?]+$/, "");
    if (MEDIA_EXT.test(clean)) return clean;
  }
  return null;
}

export function prefersReducedMotion(): boolean {
  try {
    return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return true; // cannot tell: assume yes, the cautious answer
  }
}
