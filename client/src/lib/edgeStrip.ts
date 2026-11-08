// The edge strip's state, kept in the MAIN window and pushed to the strip window.
//
// The strip is a separate tiny window, so it must not decide what a message may
// reveal or which conversations exist: the main window does that (the same
// privacy setting as the pop-up) and sends only finished text. This file is the
// pure part of that: counting unread conversations, ordering them, and what a chip
// is called. No DOM, no Tauri, so it can be tested.

import type { PopupContent } from "./notificationText";
import type { ReplyTarget } from "./quickReply";

export const STRIP_STATE_EVENT = "strip:state";
export const STRIP_READY_EVENT = "strip:ready";
export const STRIP_OPEN_EVENT = "strip:open";

/** How many conversations the strip lists; older ones fall off. */
export const MAX_CHIPS = 6;

export interface StripChip {
  key: string;
  label: string;
  count: number;
  /** Where clicking it goes. `null` for the single anonymous chip in "hidden" mode. */
  server_id: string | null;
  channel_id: number | null;
}

export interface StripPreview {
  /** Increases with every message; the strip uses it to tell a new one from a re-send. */
  id: number;
  title: string;
  body: string;
  /** Set only when the person allows replying from pop-ups and the sender is shown. */
  reply: ReplyTarget | null;
}

export interface StripState {
  /** Most recent conversation first. */
  chips: StripChip[];
  preview: StripPreview | null;
}

export interface StripNote {
  serverId: string;
  channelId: number;
  mode: PopupContent;
  sender: string | null;
  channel: string | null;
  isDm: boolean;
  title: string;
  body: string;
  reply: ReplyTarget | null;
}

/** What a chip says, never more than the person's privacy choice allows. */
export function chipLabel(mode: PopupContent, p: { sender: string | null; channel: string | null; isDm: boolean }): string {
  if (mode === "hidden") return "New messages";
  const sender = (p.sender ?? "").trim();
  if (mode === "sender") return sender || "New message";
  if (p.isDm) return sender || "Direct message";
  const channel = (p.channel ?? "").trim();
  return channel ? `#${channel}` : sender || "New message";
}

export class StripModel {
  private chips = new Map<string, StripChip>();
  private preview: StripPreview | null = null;
  private nextId = 1;

  /** A message arrived. */
  note(n: StripNote): void {
    // In "hidden" mode every conversation shares ONE chip, so the strip cannot be
    // used to learn how many different people wrote.
    const hidden = n.mode === "hidden";
    const key = hidden ? "all" : `${n.serverId}:${n.channelId}`;
    const prev = this.chips.get(key);
    // Re-insert so the Map order is "least recent first"; `state()` reverses it.
    this.chips.delete(key);
    this.chips.set(key, {
      key,
      label: chipLabel(n.mode, n),
      count: (prev?.count ?? 0) + 1,
      server_id: hidden ? null : n.serverId,
      channel_id: hidden ? null : n.channelId,
    });
    while (this.chips.size > MAX_CHIPS) {
      const oldest = this.chips.keys().next().value as string;
      this.chips.delete(oldest);
    }
    this.preview = { id: this.nextId++, title: n.title, body: n.body, reply: n.reply };
  }

  /** The person looked at Farder (or asked to clear): nothing is unread any more. */
  clear(): void {
    this.chips.clear();
    this.preview = null;
  }

  /** A reply was sent from the strip: that conversation is dealt with. */
  resolve(serverId: string, channelId: number): void {
    this.chips.delete(`${serverId}:${channelId}`);
    if (this.chips.size === 0) this.preview = null;
  }

  state(): StripState {
    return { chips: [...this.chips.values()].reverse(), preview: this.preview };
  }
}

/** Untrusted payload from the strip window asking to open a conversation. */
export function parseOpen(p: unknown): { serverId: string; channelId: number | null } | null {
  const o = p as { server_id?: unknown; channel_id?: unknown } | null;
  if (!o || typeof o.server_id !== "string" || o.server_id.length === 0 || o.server_id.length > 128) return null;
  if (o.channel_id == null) return { serverId: o.server_id, channelId: null };
  if (typeof o.channel_id !== "number" || !Number.isInteger(o.channel_id) || o.channel_id < 0) return null;
  return { serverId: o.server_id, channelId: o.channel_id };
}
