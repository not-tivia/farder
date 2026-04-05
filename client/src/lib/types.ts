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
}

export interface MemberInfo {
  public_key: { bytes: number[] };
  display_name: string;
  joined_at: number;
  role_ids: number[];
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
}

export interface ReactionGroup {
  emoji: string;
  count: number;
  me: boolean;
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

export function isDeletedUser(pk: { bytes: number[] }): boolean {
  return pk.bytes.every((b) => b === 0);
}
