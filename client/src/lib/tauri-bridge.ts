import { invoke } from "@tauri-apps/api/core";
import type { ConnectResult, SendMessageResult, MessageInfo, MemberInfo, ChannelInfo, CategoryInfo, RoleInfo, DmEntry } from "./types";

// ── Server management (no serverId needed) ───────────────────────────────────

export async function connectServer(address: string, inviteCode?: string, setupToken?: string): Promise<ConnectResult> {
  return invoke<ConnectResult>("connect_server", {
    address,
    inviteCode: inviteCode ?? null,
    setupToken: setupToken ?? null,
  });
}

export async function disconnectServer(serverId: string): Promise<void> {
  return invoke<void>("disconnect_server", { serverId });
}

export async function listServers(): Promise<{ id: string; name: string }[]> {
  return invoke<{ id: string; name: string }[]>("list_servers");
}

export async function getSavedServers(): Promise<{ id: string; name: string }[]> {
  return invoke<{ id: string; name: string }[]>("get_saved_servers");
}

// ── Identity (no serverId) ────────────────────────────────────────────────────

export async function generateKeypair(): Promise<string> {
  return invoke<string>("generate_keypair");
}

export async function loadIdentity(): Promise<string | null> {
  return invoke<string | null>("load_identity");
}

export async function getPublicKey(): Promise<string | null> {
  return invoke<string | null>("get_public_key");
}

export async function setDisplayName(name: string): Promise<void> {
  return invoke<void>("set_display_name", { name });
}

export async function getDisplayName(): Promise<string | null> {
  return invoke<string | null>("get_display_name");
}

export async function setBio(bio: string): Promise<void> {
  return invoke<void>("set_bio", { bio });
}

export async function getBio(): Promise<string | null> {
  return invoke<string | null>("get_bio");
}

export async function setProfileColor(color: string): Promise<void> {
  return invoke<void>("set_profile_color", { color });
}

export async function getProfileColor(): Promise<string | null> {
  return invoke<string | null>("get_profile_color");
}

export async function getLastServer(): Promise<string | null> {
  return invoke<string | null>("get_last_server");
}

export async function pickFile(): Promise<string | null> {
  return invoke<string | null>("pick_file");
}

export async function setAvatar(filePath: string): Promise<string> {
  return invoke<string>("set_avatar", { filePath });
}

export async function getAvatar(): Promise<string | null> {
  return invoke<string | null>("get_avatar");
}

export async function setServerAvatar(serverId: string, filePath: string): Promise<string> {
  return invoke<string>("set_server_avatar", { serverId, filePath });
}

export async function getServerAvatar(serverId: string): Promise<string | null> {
  return invoke<string | null>("get_server_avatar", { serverId });
}

export async function listFavorites(): Promise<FavoriteEntry[]> {
  return invoke<FavoriteEntry[]>("list_favorites");
}

export async function removeFavorite(id: string): Promise<void> {
  return invoke<void>("remove_favorite", { id });
}

// ── Per-server commands (all gain serverId) ───────────────────────────────────

export async function sendMessage(serverId: string, channelId: number, content: string, replyTo?: number, attachmentIds?: number[]): Promise<SendMessageResult> {
  return invoke<SendMessageResult>("send_message", { serverId, channelId, content, replyTo: replyTo ?? null, attachmentIds: attachmentIds ?? [] });
}

export async function fetchHistory(serverId: string, channelId: number, beforeId?: number, limit?: number): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("fetch_history", { serverId, channelId, beforeId: beforeId ?? null, limit: limit ?? null });
}

export async function subscribeChannels(serverId: string, channelIds: number[]): Promise<void> {
  return invoke<void>("subscribe_channels", { serverId, channelIds });
}

export async function getServerInfo(serverId: string): Promise<ConnectResult> {
  return invoke<ConnectResult>("get_server_info", { serverId });
}

export async function getMembers(serverId: string): Promise<MemberInfo[]> {
  return invoke<MemberInfo[]>("get_members", { serverId });
}

export async function addReaction(serverId: string, messageId: number, emoji: string): Promise<void> {
  return invoke<void>("add_reaction", { serverId, messageId, emoji });
}

export async function removeReaction(serverId: string, messageId: number, emoji: string): Promise<void> {
  return invoke<void>("remove_reaction", { serverId, messageId, emoji });
}

export async function createThread(serverId: string, messageId: number, name?: string): Promise<void> {
  return invoke<void>("create_thread", { serverId, messageId, name: name ?? null });
}

export async function searchMessages(serverId: string, query: string, channelId?: number, limit?: number): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("search_messages", { serverId, query, channelId: channelId ?? null, limit: limit ?? null });
}

export async function createChannel(serverId: string, name: string, channelType: string, categoryId?: number): Promise<void> {
  return invoke<void>("create_channel", { serverId, name, channelType, categoryId: categoryId ?? null });
}

export async function createCategory(serverId: string, name: string): Promise<void> {
  return invoke<void>("create_category", { serverId, name });
}

export async function deleteChannel(serverId: string, channelId: number): Promise<void> {
  return invoke<void>("delete_channel", { serverId, channelId });
}

export async function deleteCategory(serverId: string, categoryId: number): Promise<void> {
  return invoke<void>("delete_category", { serverId, categoryId });
}

export async function updateChannel(serverId: string, channelId: number, opts: {
  name?: string;
  topic?: string;
  nsfw?: boolean;
  slowModeSecs?: number;
  categoryId?: number | null;
  position?: number;
}): Promise<void> {
  const setCategory = opts.categoryId !== undefined;
  return invoke<void>("update_channel", {
    serverId,
    channelId,
    name: opts.name ?? null,
    topic: opts.topic ?? null,
    nsfw: opts.nsfw ?? null,
    slowModeSecs: opts.slowModeSecs ?? null,
    categoryId: opts.categoryId ?? null,
    setCategory,
    position: opts.position ?? null,
  });
}

export async function updateCategory(serverId: string, categoryId: number, opts: { name?: string; position?: number }): Promise<void> {
  return invoke<void>("update_category", { serverId, categoryId, ...opts });
}

export async function setChannelOverride(serverId: string, channelId: number, roleId: number, allow: number, deny: number): Promise<void> {
  return invoke<void>("set_channel_override", { serverId, channelId, roleId, allow, deny });
}

export async function openDm(serverId: string, targetKey: string): Promise<{ channel: ChannelInfo; participant: MemberInfo }> {
  return invoke("open_dm", { serverId, targetKey });
}

export async function listDms(serverId: string): Promise<DmEntry[]> {
  return invoke<DmEntry[]>("list_dms", { serverId });
}

export async function blockUser(serverId: string, targetKey: string): Promise<void> {
  return invoke("block_user", { serverId, targetKey });
}

export async function unblockUser(serverId: string, targetKey: string): Promise<void> {
  return invoke("unblock_user", { serverId, targetKey });
}

export interface InviteResult {
  code: string;
  link: string;
  deep_link: string;
}

export async function createInvite(serverId: string, maxUses?: number): Promise<InviteResult> {
  return invoke<InviteResult>("create_invite", { serverId, maxUses: maxUses ?? null });
}

export async function fetchUrl(serverId: string, url: string, channelId: number): Promise<number> {
  return invoke<number>("fetch_url", { serverId, url, channelId });
}

export async function uploadFile(serverId: string, channelId: number, filePath: string): Promise<number> {
  return invoke<number>("upload_file", { serverId, channelId, filePath });
}

export interface DownloadResult {
  data_url: string | null;
  file_name: string;
  mime_type: string;
  saved_path: string | null;
}

export async function downloadFile(serverId: string, fileId: number): Promise<DownloadResult> {
  return invoke<DownloadResult>("download_file", { serverId, fileId });
}

export interface FavoriteEntry {
  id: string;
  file_name: string;
  mime_type: string;
  data_url: string;
  source_server: string;
  original_url: string | null;
  favorited_at: number;
}

export async function addFavorite(serverId: string, fileId: number, originalUrl?: string): Promise<FavoriteEntry> {
  return invoke<FavoriteEntry>("add_favorite", { serverId, fileId, originalUrl: originalUrl ?? null });
}

export async function sendTyping(serverId: string, channelId: number): Promise<void> {
  return invoke<void>("send_typing", { serverId, channelId });
}

export async function editMessage(serverId: string, messageId: number, newContent: string): Promise<void> {
  return invoke<void>("edit_message", { serverId, messageId, newContent });
}

export async function deleteMessage(serverId: string, messageId: number): Promise<void> {
  return invoke<void>("delete_message", { serverId, messageId });
}

export async function assignRole(serverId: string, memberKey: string, roleId: number): Promise<void> {
  return invoke<void>("assign_role", { serverId, memberKey, roleId });
}

export async function createRole(serverId: string, name: string, permissions: number, color?: string): Promise<void> {
  return invoke("create_role", { serverId, name, permissions, color: color ?? null });
}

export async function deleteRole(serverId: string, roleId: number): Promise<void> {
  return invoke("delete_role", { serverId, roleId });
}

export async function removeRole(serverId: string, memberKey: string, roleId: number): Promise<void> {
  return invoke<void>("remove_role", { serverId, memberKey, roleId });
}

export async function requestDeletion(serverId: string): Promise<void> {
  return invoke<void>("request_deletion", { serverId });
}

export async function cancelDeletion(serverId: string): Promise<void> {
  return invoke<void>("cancel_deletion", { serverId });
}

export async function getDeletionStatus(serverId: string): Promise<any> {
  return invoke("get_deletion_status", { serverId });
}

export async function saveTempAudio(data: string): Promise<string> {
  return invoke<string>("save_temp_audio", { data });
}

export async function startRecording(): Promise<void> {
  return invoke<void>("start_recording");
}

export async function stopRecording(): Promise<string> {
  return invoke<string>("stop_recording");
}

export async function showNotification(title: string, body: string): Promise<void> {
  return invoke<void>("show_notification", { title, body });
}

export interface NotificationPrefs {
  dmNotifications: "all" | "specific" | "none";
  dmAllowedUsers: string[];
  servers: Record<string, { mode: "all" | "mentions" | "none" }>;
  mentionNotifications: boolean;
  keywords: string[];
  mutedUsers: string[];
  // Event triggers
  notifyOnMemberJoin?: boolean;
  notifyOnMemberLeave?: boolean;
  notifyOnReaction?: boolean;
  notifyOnThreadReply?: boolean;
}

export async function getNotificationPrefs(): Promise<NotificationPrefs> {
  return invoke<NotificationPrefs>("get_notification_prefs");
}

export async function saveNotificationPrefs(prefs: NotificationPrefs): Promise<void> {
  return invoke<void>("save_notification_prefs", { prefs });
}

// ── DM E2EE ───────────────────────────────────────────────────────────────────

/**
 * Encrypt a plaintext message for a DM peer.
 * Returns a hex-encoded AES-256-GCM ciphertext (nonce prepended).
 */
export async function dmEncrypt(theirPublicKey: string, plaintext: string): Promise<string> {
  return invoke<string>("dm_encrypt", { theirPublicKey, plaintext });
}

/**
 * Decrypt a hex-encoded ciphertext received in a DM channel.
 * Throws if decryption fails (wrong key, tampered data, or plaintext message).
 */
export async function dmDecrypt(theirPublicKey: string, ciphertextHex: string): Promise<string> {
  return invoke<string>("dm_decrypt", { theirPublicKey, ciphertextHex });
}

// ── Voice channel commands ────────────────────────────────────────────────────

export interface VoiceMember {
  public_key: { bytes: number[] };
  display_name: string;
  joined_at: number;
}

export async function joinVoice(serverId: string, channelId: number): Promise<void> {
  return invoke("join_voice", { serverId, channelId });
}

export async function leaveVoice(serverId: string, channelId: number): Promise<void> {
  return invoke("leave_voice", { serverId, channelId });
}

export async function getVoiceState(serverId: string, channelId: number): Promise<VoiceMember[]> {
  return invoke("get_voice_state", { serverId, channelId });
}

// ---------------------------------------------------------------------------
// Local server management
// ---------------------------------------------------------------------------

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
}

export interface ManagedServer {
  name: string;
  port: number;
  data_dir: string;
  template: string;
  privacy: string;
}

export async function createLocalServer(
  name: string,
  template: string,
  privacy: string,
  iconPath?: string,
): Promise<{
  address: string;
  server_name: string;
  member_count: number;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
}> {
  return invoke("create_local_server", {
    name,
    template,
    privacy,
    iconPath: iconPath ?? null,
  });
}

export async function stopLocalServer(port: number): Promise<void> {
  return invoke("stop_local_server", { port });
}

export async function getLocalServers(): Promise<ManagedServer[]> {
  return invoke("get_local_servers");
}

export async function listTemplates(): Promise<TemplateInfo[]> {
  return invoke("list_templates");
}
