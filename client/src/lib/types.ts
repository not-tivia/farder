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

export type PresenceKind = "Music" | "Game";
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
