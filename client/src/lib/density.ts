// Message density: how much room each chat message takes. Stored on this device
// only. The look is one stylesheet keyed on `<html data-density="...">` that uses
// the existing message classes and the theme's own variables, so it works in
// every theme without editing each theme file.

export const DENSITIES = ["roomy", "normal", "compact", "irc", "tiny"] as const;
export type Density = (typeof DENSITIES)[number];

export const DEFAULT_DENSITY: Density = "normal";
export const DENSITY_CHANGED = "farder:density-changed";
const KEY = "farder.density";

export function isDensity(v: unknown): v is Density {
  return typeof v === "string" && (DENSITIES as readonly string[]).includes(v);
}

export function getDensity(): Density {
  try {
    const raw = localStorage.getItem(KEY);
    return isDensity(raw) ? raw : DEFAULT_DENSITY;
  } catch {
    return DEFAULT_DENSITY; // storage blocked: behave as today's layout
  }
}

export function applyDensity(d: Density, root: { dataset: Record<string, string | undefined> } = document.documentElement): void {
  root.dataset.density = d;
}

export function setDensity(d: Density): void {
  try { localStorage.setItem(KEY, d); } catch { /* does not persist */ }
  applyDensity(d);
  window.dispatchEvent(new CustomEvent(DENSITY_CHANGED));
}

/**
 * Whether consecutive messages from one person are folded under a single name.
 * The dense modes show the name on every line instead: a one-line row has no
 * header to fold under, and IRC has always repeated the nick.
 */
export function foldsRepeatedAuthors(d: Density): boolean {
  return d === "roomy" || d === "normal";
}

/** The short clock shown inline in IRC mode (no "Today at ..."). */
export function shortClock(tsSeconds: number): string {
  try {
    return new Date(tsSeconds * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  } catch {
    return "";
  }
}

export const DENSITY_INFO: Record<Density, { label: string; description: string }> = {
  roomy: { label: "Roomy", description: "Larger pictures and more space between messages." },
  normal: { label: "Normal", description: "How Farder has always looked." },
  compact: { label: "Compact", description: "Name and message on one line with a small picture. The time shows when you point at a message." },
  irc: { label: "IRC", description: "No pictures. [time] <name> message, in a monospace font, like mIRC." },
  tiny: { label: "Tiny", description: "Name and message only, in small type. The most messages per screen." },
};

/** The stylesheet. Colours come only from theme variables. */
export const DENSITY_CSS = `
/* roomy */
html[data-density="roomy"] .message-list { gap: 6px; }
html[data-density="roomy"] .message:not(.grouped) { padding-top: 9px; padding-bottom: 6px; }
html[data-density="roomy"] .message-avatar { width: 38px; height: 38px; font-size: 16px; }
html[data-density="roomy"] .message-content { font-size: 14px; line-height: 1.5; }
html[data-density="roomy"] .message-author { font-size: 14px; }

/* shared by compact / irc / tiny: one row per message */
html[data-density="compact"] .message-list,
html[data-density="irc"] .message-list,
html[data-density="tiny"] .message-list { gap: 0; padding-top: 4px; padding-bottom: 4px; }
html[data-density="compact"] .message,
html[data-density="irc"] .message,
html[data-density="tiny"] .message {
  flex-direction: row; flex-wrap: wrap; align-items: baseline; column-gap: 6px;
  padding: 1px 6px; margin-top: 0; border-top: none;
}
html[data-density="compact"] .message-header,
html[data-density="irc"] .message-header,
html[data-density="tiny"] .message-header { flex: none; align-items: baseline; gap: 6px; }
html[data-density="compact"] .message-content,
html[data-density="irc"] .message-content,
html[data-density="tiny"] .message-content { flex: 1 1 0; min-width: 0; line-height: 1.3; }
/* things that are blocks of their own keep their own line, a little indented */
html[data-density="compact"] .message-reply-context,
html[data-density="irc"] .message-reply-context,
html[data-density="tiny"] .message-reply-context,
html[data-density="compact"] .reaction-bar,
html[data-density="irc"] .reaction-bar,
html[data-density="tiny"] .reaction-bar,
html[data-density="compact"] .thread-link,
html[data-density="irc"] .thread-link,
html[data-density="tiny"] .thread-link,
html[data-density="compact"] .message-attachments,
html[data-density="irc"] .message-attachments,
html[data-density="tiny"] .message-attachments,
html[data-density="compact"] .invite-embeds,
html[data-density="irc"] .invite-embeds,
html[data-density="tiny"] .invite-embeds,
html[data-density="compact"] .linked-widget-embeds,
html[data-density="irc"] .linked-widget-embeds,
html[data-density="tiny"] .linked-widget-embeds,
html[data-density="compact"] .link-embeds,
html[data-density="irc"] .link-embeds,
html[data-density="tiny"] .link-embeds { flex: 0 0 100%; box-sizing: border-box; margin-left: 24px; }
html[data-density="irc"] .message-reply-context,
html[data-density="tiny"] .message-reply-context,
html[data-density="irc"] .reaction-bar,
html[data-density="tiny"] .reaction-bar,
html[data-density="irc"] .thread-link,
html[data-density="tiny"] .thread-link,
html[data-density="irc"] .message-attachments,
html[data-density="tiny"] .message-attachments,
html[data-density="irc"] .invite-embeds,
html[data-density="tiny"] .invite-embeds,
html[data-density="irc"] .linked-widget-embeds,
html[data-density="tiny"] .linked-widget-embeds { margin-left: 8px; }
/* link previews are the bulky part; the link itself is still in the text */
html[data-density="irc"] .link-embeds,
html[data-density="tiny"] .link-embeds { display: none; }
html[data-density="compact"] .grouped-timestamp,
html[data-density="irc"] .grouped-timestamp,
html[data-density="tiny"] .grouped-timestamp { display: none; }

/* the time appears over the right edge when you point at the message */
html[data-density="compact"] .message-timestamp,
html[data-density="tiny"] .message-timestamp {
  position: absolute; right: 6px; top: 50%; transform: translateY(-50%);
  opacity: 0; pointer-events: none; padding: 0 4px; background: var(--xp-panel-bg);
}
html[data-density="compact"] .message:hover .message-timestamp,
html[data-density="tiny"] .message:hover .message-timestamp { opacity: 1; }

/* compact: small picture, normal type */
html[data-density="compact"] .message-avatar { width: 18px; height: 18px; font-size: 10px; margin-right: 0; align-self: center; }
html[data-density="compact"] .message-author,
html[data-density="compact"] .message-content { font-size: 13px; }

/* irc: [time] <name> text */
html[data-density="irc"] .message { font-family: ui-monospace, "Cascadia Mono", Consolas, "Liberation Mono", monospace; }
html[data-density="irc"] .message-avatar { display: none; }
html[data-density="irc"] .message-author,
html[data-density="irc"] .message-content,
html[data-density="irc"] .message-timestamp { font-size: 12px; }
html[data-density="irc"] .message-author { font-weight: normal; }
html[data-density="irc"] .message-author::before { content: "<"; }
html[data-density="irc"] .message-author::after { content: ">"; }
html[data-density="irc"] .message-timestamp { order: -1; }
html[data-density="irc"] .message-timestamp::before { content: "["; }
html[data-density="irc"] .message-timestamp::after { content: "]"; }

/* tiny: name: text, small */
html[data-density="tiny"] .message-avatar { display: none; }
html[data-density="tiny"] .message-header { gap: 4px; }
html[data-density="tiny"] .message-author,
html[data-density="tiny"] .message-content { font-size: 11px; }
html[data-density="tiny"] .message-author::after { content: ":"; }
html[data-density="tiny"] .message-list { padding-left: 8px; padding-right: 8px; }
`;

/** Called once at startup: injects the stylesheet and applies the saved choice. */
export function initDensity(): void {
  if (!document.getElementById("density-css")) {
    const style = document.createElement("style");
    style.id = "density-css";
    style.textContent = DENSITY_CSS;
    document.head.appendChild(style);
  }
  applyDensity(getDensity());
}
