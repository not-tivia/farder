// What a notification is ALLOWED to say, decided in one place so no path can
// leak more than the person chose.
//
// Pop-ups appear over other programs, on shared screens, in screen recordings and
// (for the system style) in the OS notification history. People differ a lot in
// how much of a message they want there, so this is a setting, not a default
// imposed on everyone.

/** `full`: who + where + the words. `sender`: only who. `hidden`: nothing about it. */
export type PopupContent = "full" | "sender" | "hidden";

export const POPUP_CONTENT_MODES: readonly PopupContent[] = ["full", "sender", "hidden"];

export function normalizePopupContent(v: unknown): PopupContent {
  return v === "sender" || v === "hidden" ? v : "full";
}

export interface NotificationParts {
  /** The sender's display name, if known. */
  sender?: string | null;
  /** The channel the message is in, without the `#`, if known. */
  channel?: string | null;
  /** The message text. */
  text: string;
}

export interface NotificationText {
  title: string;
  body: string;
  /** Whether a media preview may accompany it. Only ever with the full text. */
  allowMedia: boolean;
}

const MAX_BODY = 120;

function clean(s: string | null | undefined): string {
  // Names are other people's input: no control characters or line breaks.
  return Array.from(s ?? "").filter((c) => !/[\p{Cc}\p{Cf}]/u.test(c)).join("").trim().slice(0, 64);
}

export function buildNotificationText(mode: PopupContent, p: NotificationParts): NotificationText {
  const sender = clean(p.sender);
  const channel = clean(p.channel);
  switch (mode) {
    case "hidden":
      // Not even who it is from: a screen share or a glance over the shoulder
      // learns only that something arrived.
      return { title: "Farder", body: "New message", allowMedia: false };
    case "sender":
      return { title: "Farder", body: sender ? `New message from ${sender}` : "New message", allowMedia: false };
    case "full":
    default: {
      const who = sender || "Farder";
      return {
        title: channel && sender ? `${who} in #${channel}` : who,
        body: Array.from(p.text).slice(0, MAX_BODY).join(""),
        allowMedia: true,
      };
    }
  }
}
