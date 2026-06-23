// Remembered, clamped width of the member sidebar (client-side, draggable).
const KEY = "farder.memberSidebarWidth";
export const MEMBER_SIDEBAR_DEFAULT = 180;
export const MEMBER_SIDEBAR_MIN = 160;
export const MEMBER_SIDEBAR_MAX = 480;

export function clampMemberWidth(w: number): number {
  return Math.min(MEMBER_SIDEBAR_MAX, Math.max(MEMBER_SIDEBAR_MIN, Math.round(w)));
}

/** Saved width, clamped; falls back to the default on missing/invalid/error. */
export function getMemberSidebarWidth(): number {
  try {
    const n = parseInt(localStorage.getItem(KEY) ?? "", 10);
    return Number.isFinite(n) ? clampMemberWidth(n) : MEMBER_SIDEBAR_DEFAULT;
  } catch { return MEMBER_SIDEBAR_DEFAULT; }
}

export function setMemberSidebarWidth(w: number): void {
  try { localStorage.setItem(KEY, String(clampMemberWidth(w))); } catch { /* ignore */ }
}
