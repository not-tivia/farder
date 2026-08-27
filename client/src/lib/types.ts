export interface ChannelInfo {
  id: number;
  name: string;
  channel_type: "Text" | "Announcement" | "Thread" | "Dm" | "Voice";
  category_id: number | null;
  position: number;
  topic: string | null;
  nsfw: boolean;
  slow_mode_secs: number | null;
  retention_secs: number | null;
  thread_parent_message_id: number | null;
  /** Rung-2 client-side channel class. Absent on raw v1 frames — Rust
   *  `ChannelInfo` has no `class`; it rides in `ChannelInfoV2` and the client
   *  flattens it onto each entry on connect. `undefined` means plaintext. */
  class?: ChannelClass;
}

/** A channel's content class — immutable, set at creation (Rung-2).
 *  Mirrors Rust `farder_crypto::event_log::ChannelClass`. */
export type ChannelClass = "Plaintext" | "E2ee";

/** `ChannelInfo` plus its content class (Rung-2 v2 surface). The wire shape is
 *  `{ base: ChannelInfo, class: ChannelClass }` — serde serializes the Rust
 *  `ChannelInfoV2` struct by field name, so the class rides alongside `base`,
 *  never flattened into it. */
export interface ChannelInfoV2 {
  base: ChannelInfo;
  class: ChannelClass;
}

export interface CategoryInfo {
  id: number;
  name: string;
  position: number;
}

export interface RoleInfo {
  id: number;
  name: string;
  permissions: number;
  color: string | null;
  position: number;
  hoist: boolean;
}

export type PresenceKind = "Music" | "Game" | "Ticker";
export interface Presence { kind: PresenceKind; details: string; state?: string | null }

export interface MemberInfo {
  public_key: { bytes: number[] };
  display_name: string;
  joined_at: number;
  role_ids: number[];
  timeout_until?: number | null;
  timeout_reason?: string | null;
  profile_hash?: string | null;
  presence?: Presence | null;
  is_bot?: boolean;
}

export interface AttachmentInfo {
  id: number;
  file_id: number;
  name: string;
  size: number;
  mime_type: string;
  width: number | null;
  height: number | null;
  duration_secs: number | null;
  content_hash?: string;
  redacted_by_moderator?: boolean | null;
}

export interface ReactionGroup {
  emoji: string;
  count: number;
  me: boolean;
  file_id?: number;
}

export interface MessageInfo {
  id: number;
  channel_id: number;
  author: { bytes: number[] };
  content: string;
  timestamp: number;
  edited_at: number | null;
  reply_to: number | null;
  pinned: boolean;
  attachments: AttachmentInfo[];
  reactions: ReactionGroup[];
  thread_id: number | null;
  thread_message_count: number;
  /** Display-name override for webhook-posted messages; null for normal member messages. */
  author_name_override?: string | null;
  /** Badge label shown next to the author name for bot/webhook posts ("WEBHOOK", "BOT", etc.). */
  author_badge?: string | null;
  /** Server-written widget marker JSON (e.g. `{"type":"poll","id":7}`); null/absent
   *  for normal messages. Treat as untrusted: try/catch parse, numeric id check. */
  widget?: string | null;
  /** Rung-2 sealed-row fields, absent on v1/plaintext rows. A sealed row has
   *  empty `content` and ciphertext in `sealed`; `event_hash` cites the log
   *  event for reply/edit/delete. `is_e2ee` undefined = plaintext. */
  is_e2ee?: boolean;
  sealed?: number[] | null;
  event_hash?: string | null;
}

/** A message plus the sealed-row fields a v1 client cannot render (Rung-2 v2
 *  surface). For a sealed row `base.content` is "" — the server holds ciphertext
 *  and cannot fill it — and the payload rides in `sealed`. `event_hash` is the
 *  hash a client cites when replying/editing/deleting over the log.
 *  Mirrors Rust `MessageInfoV2 { base, is_e2ee, sealed, event_hash }`. */
export interface MessageInfoV2 {
  base: MessageInfo;
  is_e2ee: boolean;
  sealed: number[] | null;
  event_hash: string | null;
}

/** Live poll state, broadcast whole on every change (`server:poll_updated`) and
 *  returned by `getPoll`. Shared state only — my own vote rides separately as
 *  `my_vote` in the `getPoll` response. */
export interface PollInfo {
  id: number;
  channel_id: number;
  message_id: number;
  /** Serde-serialized PublicKey (same shape as MessageInfo.author); use publicKeyToString(). */
  creator: { bytes: number[] };
  question: string;
  options: string[];
  /** Vote counts aligned index-for-index with `options`. */
  counts: number[];
  total_votes: number;
  created_at: number;
  closes_at: number | null;
  closed: boolean;
}

/** Live giveaway state, broadcast whole on every change (`server:giveaway_updated`)
 *  and returned by `getGiveaway`. Shared state only — whether I entered rides
 *  separately as `my_entered` in the `getGiveaway` response. */
export interface GiveawayInfo {
  id: number;
  channel_id: number;
  message_id: number;
  /** Serde-serialized PublicKey (same shape as MessageInfo.author); use publicKeyToString(). */
  creator: { bytes: number[] };
  prize: string;
  ends_at: number;
  status: "open" | "ended" | "cancelled";
  /** Live entry count; entrant identities never leave the server. */
  entry_count: number;
  /** Winner public key as its "vk_<hex>" string form (mapped in the Tauri layer); null until drawn / when no entries. */
  winner: string | null;
  /** Server-resolved display name when ended with a winner still on the roster; fall back to the short key form. */
  winner_name: string | null;
}

/** Live event state, broadcast whole on every change (`server:event_updated`)
 *  and returned by `getEvent`. Shared state only — my own RSVP rides separately
 *  as `my_rsvp` in the `getEvent` response.
 *
 *  The attendee roster is DISPLAY NAMES ONLY (never public keys), capped at 10
 *  per option server-side; render "and N more" from `count - names.length`. */
export interface EventInfo {
  id: number;
  channel_id: number;
  message_id: number;
  /** Serde-serialized PublicKey (same shape as MessageInfo.author); use publicKeyToString(). */
  creator: { bytes: number[] };
  title: string;
  description: string | null;
  location: string | null;
  /** Absolute unix secs — no timezone travels with it; render with toLocaleString(). */
  starts_at: number;
  /** Secs before start for the lead-time DM: 900 | 3600 | 86400; null = none. */
  remind_lead: number | null;
  status: "upcoming" | "started" | "cancelled";
  going_count: number;
  maybe_count: number;
  no_count: number;
  /** Capped at 10 each by the server. */
  going_names: string[];
  maybe_names: string[];
  no_names: string[];
}

/** Full event state: shared EventInfo plus my own RSVP (self-only). */
export interface EventState {
  event: EventInfo;
  my_rsvp: string | null;
}

/** One of MY pending reminders (`listMyReminders`). Owner-scoped server-side by
 *  the connection key; the text is never broadcast and posts nothing. */
export interface ReminderInfo {
  id: number;
  text: string;
  /** Absolute unix secs; render with toLocaleString(). */
  due_at: number;
  created_at: number;
  /** Where it was set — link-back context only. */
  channel_id: number;
}

export interface CommandInfo {
  id: number;
  trigger: string;
  description: string;
  takes_arg: boolean;
  /** Command kind: "text" | "api" | "poll" | "giveaway" | "event" | "reminder".
   *  Empty string when talking to an old server that predates the field (serde
   *  default). Builder-form kinds (poll/giveaway/event/reminder) open a modal
   *  instead of raw text. */
  kind: string;
}

export interface DmEntry {
  channel: ChannelInfo;
  participant: MemberInfo;
  last_message: MessageInfo | null;
}

export interface ConnectResult {
  server_name: string;
  member_count: number;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
  owner_public_key?: { bytes: number[] } | null;
  relayed?: boolean;
  server_id?: string | null;
}

/** The v2 server-info surface (`getServerInfoV2`): `ConnectResult`'s shape
 *  minus the connection-only `relayed` flag, with a class-carrying channel
 *  list. Mirrors Rust `ServerInfoV2Result`. */
export interface ServerInfoV2 {
  server_name: string;
  member_count: number;
  channels: ChannelInfoV2[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
  owner_public_key?: { bytes: number[] } | null;
  server_id?: string | null;
}

/** Flatten a v2 channel into the client-side `ChannelInfo` entry, merging its
 *  `class` onto `base`. The `channels` state entries are the flattened form. */
export function flattenChannelInfoV2(channels: ChannelInfoV2[]): ChannelInfo[] {
  return channels.map((c) => ({ ...c.base, class: c.class }));
}

/** Flatten a v2 message row into the client-side `MessageInfo` entry, merging
 *  the sealed-row fields onto `base`. Plaintext rows pass through with the
 *  fields undefined. */
export function flattenMessageInfoV2(messages: MessageInfoV2[]): MessageInfo[] {
  return messages.map((m) => ({ ...m.base, is_e2ee: m.is_e2ee, sealed: m.sealed, event_hash: m.event_hash }));
}

/** Ask "is this channel E2EE?" — unknown/absent class defaults to plaintext. */
export function isE2eeChannel(channel: ChannelInfo | null | undefined): boolean {
  return (channel?.class ?? "Plaintext") === "E2ee";
}

export interface SendMessageResult {
  id: number;
  timestamp: number;
}

export interface ServerListEntry {
  id: string;
  name: string;
  connected: boolean;
  unreadCount: number;
  hasMention: boolean;
}

export const DELETED_USER_KEY: number[] = new Array(32).fill(0);

export function publicKeyToString(pk: { bytes: number[] }): string {
  const hex = pk.bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
  return "vk_" + hex;
}

/**
 * Friendly display name for a member. The server auto-assigns "vk_<8 hex>" to
 * anyone who hasn't set a name; show a human-friendly placeholder for those
 * (and for empty names) instead of the raw key.
 */
export function memberDisplayName(name: string | null | undefined): string {
  const n = (name ?? "").trim();
  if (!n || /^vk_[0-9a-f]{8}$/.test(n)) return "Anonymous"; // server emits lowercase hex
  return n;
}

export function isDeletedUser(pk: { bytes: number[] }): boolean {
  return pk.bytes.every((b) => b === 0);
}

export interface BannedMember {
  // Serde-serialized PublicKey: a { bytes } object, not a string. Use
  // publicKeyToString() before passing to commands or using as a React key.
  public_key: { bytes: number[] };
  display_name: string;
  ban_reason?: string;
  banned_at: number;
}

export interface BotAlertInfo {
  id: number;
  metric: string;
  comparator: string;
  threshold: number;
}

export interface WebhookInfo {
  id: number;
  channel_id: number;
  name: string;
}

/** Return value for createWebhook / regenerateWebhookToken — includes the relay
 *  server_id_hex needed to build the ingest URL. Shown once; never retrievable. */
export interface WebhookTokenResult {
  id: number;
  token: string;
  server_id_hex: string | null;
}
