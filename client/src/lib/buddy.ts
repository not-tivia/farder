// The buddy list and its chat windows (MSN-style), the pure part.
//
// Both are separate small windows. The MAIN window owns every conversation and
// decides what they say; it pushes finished text to them (`buddy:state`,
// `chat:state`) and they only ASK for things (open a conversation, send a line,
// mark it read). Nothing here touches the DOM or Tauri, so it can be tested.

import type { AttachmentInfo, ChannelInfo, DmEntry, MemberInfo, MessageInfo, SealedDecryptEntry } from "./types";
import { isDeletedUser, memberDisplayName, publicKeyToString } from "./types";
import { displayGarble } from "./garble";

export const BUDDY_STATE_EVENT = "buddy:state";
export const BUDDY_READY_EVENT = "buddy:ready";
export const CHAT_STATE_EVENT = "chat:state";
export const CHAT_READY_EVENT = "chat:ready";
export const CHAT_READ_EVENT = "chat:read";

/** Newest messages a chat window shows. */
export const CHAT_HISTORY = 60;
/** Unread counts stop at this ("99+"). */
export const UNREAD_CAP = 99;

/** The slice of one server's state these builders read. */
export interface ServerView {
  serverName: string;
  channels: ChannelInfo[];
  dms: DmEntry[];
  members: MemberInfo[];
  messages: Record<number, MessageInfo[]>;
  readState: Record<number, number>;
  currentChannelId: number | null;
  sealedDecrypts?: Record<number, SealedDecryptEntry>;
}

export interface BuddyEntry {
  server_id: string;
  channel_id: number;
  label: string;
  kind: "person" | "channel";
  server_name: string;
  unread: number;
  encrypted: boolean;
}

export interface BuddyState {
  entries: BuddyEntry[];
}

function unreadCount(view: ServerView, channelId: number, ownPk: string | null): number {
  if (channelId === view.currentChannelId) return 0;
  const seen = view.readState[channelId] ?? 0;
  let n = 0;
  for (const m of view.messages[channelId] ?? []) {
    if (m.id > seen && (!ownPk || publicKeyToString(m.author) !== ownPk)) n++;
  }
  return Math.min(n, UNREAD_CAP);
}

function lastId(view: ServerView, channelId: number): number {
  return (view.messages[channelId] ?? []).reduce((m, x) => Math.max(m, x.id), 0);
}

/** Every person (direct message) and text channel across the connected servers. */
export function buildBuddyState(servers: Record<string, ServerView>, order: string[], ownPk: string | null): BuddyState {
  const people: (BuddyEntry & { last: number })[] = [];
  const channels: BuddyEntry[] = [];
  for (const sid of order) {
    const v = servers[sid];
    if (!v) continue;
    for (const dm of v.dms) {
      people.push({
        server_id: sid,
        channel_id: dm.channel.id,
        label: memberDisplayName(dm.participant.display_name),
        kind: "person",
        server_name: v.serverName,
        unread: unreadCount(v, dm.channel.id, ownPk),
        encrypted: false,
        last: lastId(v, dm.channel.id),
      });
    }
    for (const ch of [...v.channels].sort((a, b) => a.position - b.position)) {
      if (ch.channel_type !== "Text" && ch.channel_type !== "Announcement") continue;
      channels.push({
        server_id: sid,
        channel_id: ch.id,
        label: ch.name,
        kind: "channel",
        server_name: v.serverName,
        unread: unreadCount(v, ch.id, ownPk),
        encrypted: (ch.class ?? "Plaintext") === "E2ee",
      });
    }
  }
  // People first, the one who wrote last on top; then channels in server order.
  people.sort((a, b) => b.last - a.last);
  return { entries: [...people.map(({ last: _l, ...e }) => e), ...channels] };
}

// --- chat windows -----------------------------------------------------------

export interface ChatLine {
  id: number;
  author: string;
  mine: boolean;
  /** Unix seconds, as stored. */
  time: number;
  text: string;
}

export interface ChatState {
  server_id: string;
  channel_id: number;
  title: string;
  kind: "person" | "channel";
  /** False for a conversation that cannot be answered from here (with `reason`). */
  can_send: boolean;
  reason: string | null;
  lines: ChatLine[];
  /** Highest message id in the conversation (the window asks to mark up to it as read). */
  last_id: number;
}

function attachmentNote(a: AttachmentInfo[]): string {
  if (a.length === 0) return "";
  return a.map((x) => `[file: ${x.name}]`).join(" ");
}

/** What one message says, the way the main window would draw it. */
export function lineText(m: MessageInfo, isOwn: boolean, sealed: SealedDecryptEntry | undefined): string {
  if (isDeletedUser(m.author)) return "This message has been deleted.";
  const isSealedRow = m.is_e2ee === true && m.sealed != null && m.content === "";
  let text: string;
  if (isSealedRow) {
    if (sealed?.kind === "decrypted") text = sealed.content;
    else if (sealed?.kind === "undecryptable") return "(encrypted message, could not be decrypted)";
    else return "(encrypted message)";
  } else {
    text = m.content;
  }
  // A garble rule that covered the author draws other people's copy garbled; the
  // author keeps their own words. A small window must not undo that.
  if (m.garble && !isOwn) text = displayGarble(text, m.garble, m.event_hash ?? `m${m.id}`);
  const files = attachmentNote(m.attachments ?? []);
  return [text, files].filter(Boolean).join(" ");
}

/** The transcript of one conversation, or null when it does not exist (any more). */
export function buildChatState(view: ServerView | undefined, serverId: string, channelId: number, ownPk: string | null): ChatState | null {
  if (!view) return null;
  const dm = view.dms.find((d) => d.channel.id === channelId);
  const channel = view.channels.find((c) => c.id === channelId);
  if (!dm && !channel) return null;
  const encrypted = !dm && (channel?.class ?? "Plaintext") === "E2ee";
  const names = new Map(view.members.map((m) => [publicKeyToString(m.public_key), memberDisplayName(m.display_name)]));
  const msgs = [...(view.messages[channelId] ?? [])].sort((a, b) => a.id - b.id).slice(-CHAT_HISTORY);
  const lines: ChatLine[] = msgs.map((m) => {
    const pk = publicKeyToString(m.author);
    const mine = !!ownPk && pk === ownPk;
    return {
      id: m.id,
      author: m.author_name_override || (mine ? "You" : names.get(pk) ?? "Someone"),
      mine,
      time: m.timestamp,
      text: lineText(m, mine, view.sealedDecrypts?.[m.id]),
    };
  });
  return {
    server_id: serverId,
    channel_id: channelId,
    title: dm ? memberDisplayName(dm.participant.display_name) : `#${channel!.name}`,
    kind: dm ? "person" : "channel",
    can_send: !encrypted,
    reason: encrypted ? "This channel is encrypted. Reply from the Farder window." : null,
    lines,
    last_id: msgs.reduce((m, x) => Math.max(m, x.id), 0),
  };
}

// --- untrusted requests from the small windows --------------------------------

/** Chat windows are labelled `chat-<8 hex>-<channel id>` (see buddy.rs). */
export function isChatLabel(label: unknown): label is string {
  return typeof label === "string" && /^chat-[0-9a-f]{8}-\d{1,15}$/.test(label);
}

export interface ChatRef { label: string; serverId: string; channelId: number }

/** `chat:ready` / `chat:read` payloads come from another window: check them. */
export function parseChatRef(p: unknown): ChatRef | null {
  const o = p as { label?: unknown; server_id?: unknown; channel_id?: unknown } | null;
  if (!o || !isChatLabel(o.label)) return null;
  if (typeof o.server_id !== "string" || o.server_id.length === 0 || o.server_id.length > 1024) return null;
  if (typeof o.channel_id !== "number" || !Number.isInteger(o.channel_id) || o.channel_id < 0) return null;
  return { label: o.label, serverId: o.server_id, channelId: o.channel_id };
}

export function parseChatRead(p: unknown): (ChatRef & { lastId: number }) | null {
  const ref = parseChatRef(p);
  const last = (p as { last_id?: unknown } | null)?.last_id;
  if (!ref || typeof last !== "number" || !Number.isInteger(last) || last < 0) return null;
  return { ...ref, lastId: last };
}
