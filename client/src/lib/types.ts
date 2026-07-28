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

export interface CommandInfo {
  id: number;
  trigger: string;
  description: string;
  takes_arg: boolean;
  /** Command kind: "text" | "api" | "poll" | "giveaway". Empty string when
   *  talking to an old server that predates the field (serde default).
   *  Builder-form kinds (poll/giveaway) open a modal instead of raw text. */
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
