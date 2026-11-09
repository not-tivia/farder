// Window layout: the full app, or the small single-window layout (a conversation
// list and one chat, nothing else). Stored on this device only. The look is one
// stylesheet keyed on `<html data-layout="...">` that hides the wide-layout parts
// and uses only existing classes and theme variables, so it works in every theme.
// Resizing the window itself is Rust's job (`set_window_layout`).

export const LAYOUTS = ["full", "small"] as const;
export type Layout = (typeof LAYOUTS)[number];

export const DEFAULT_LAYOUT: Layout = "full";
export const LAYOUT_CHANGED = "farder:layout-changed";
const KEY = "farder.layout";

export function isLayout(v: unknown): v is Layout {
  return v === "full" || v === "small";
}

export function getLayout(): Layout {
  try {
    const raw = localStorage.getItem(KEY);
    return isLayout(raw) ? raw : DEFAULT_LAYOUT;
  } catch {
    return DEFAULT_LAYOUT; // storage blocked: behave as the full app
  }
}

export function applyLayout(l: Layout, root: { dataset: Record<string, string | undefined> } = document.documentElement): void {
  root.dataset.layout = l;
}

/** Save and apply (no window resizing here: see `lib/layoutWindow.ts`). */
export function setLayout(l: Layout): void {
  try { localStorage.setItem(KEY, l); } catch { /* does not persist */ }
  applyLayout(l);
  window.dispatchEvent(new CustomEvent(LAYOUT_CHANGED));
}

export const LAYOUT_INFO: Record<Layout, { label: string; description: string }> = {
  full: { label: "Full window", description: "Servers, channels, chat and member list side by side. How Farder has always looked." },
  small: {
    label: "Small window",
    description: "One narrow list of conversations and one chat, like an old messenger. Shrinks the window; voice channels and the member list are in the full window.",
  },
};

/**
 * The stylesheet. The list itself (`.compact-list`) reuses the channel list's
 * classes, so it takes each theme's own look; only layout lives here.
 */
export const LAYOUT_CSS = `
html[data-layout="small"] .server-strip,
html[data-layout="small"] .channel-sidebar:not(.compact-list),
html[data-layout="small"] .member-sidebar,
html[data-layout="small"] .dm-panel { display: none; }
html[data-layout="small"] .compact-list { width: 168px; min-width: 168px; }
html[data-layout="small"] .chat-panel { min-width: 0; }
html[data-layout="small"] .channel-header-topic { display: none; }
`;
