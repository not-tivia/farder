import { invoke } from "@tauri-apps/api/core";
import type { ConnectResult, SendMessageResult, MessageInfo, MemberInfo, ChannelInfo, DmEntry } from "./types";

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

export async function getLastServer(): Promise<string | null> {
  return invoke<string | null>("get_last_server");
}

export async function connectServer(
  address: string,
  inviteCode?: string,
  setupToken?: string,
): Promise<ConnectResult> {
  return invoke<ConnectResult>("connect_server", {
    address,
    inviteCode: inviteCode ?? null,
    setupToken: setupToken ?? null,
  });
}

export async function disconnectServer(): Promise<void> {
  return invoke<void>("disconnect_server");
}

export async function sendMessage(
  channelId: number,
  content: string,
  replyTo?: number,
  attachmentIds?: number[],
): Promise<SendMessageResult> {
  return invoke<SendMessageResult>("send_message", {
    channelId,
    content,
    replyTo: replyTo ?? null,
    attachmentIds: attachmentIds ?? [],
  });
}

export async function fetchHistory(
  channelId: number,
  beforeId?: number,
  limit?: number,
): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("fetch_history", {
    channelId,
    beforeId: beforeId ?? null,
    limit: limit ?? null,
  });
}

export async function subscribeChannels(channelIds: number[]): Promise<void> {
  return invoke<void>("subscribe_channels", { channelIds });
}

export async function getMembers(): Promise<MemberInfo[]> {
  return invoke<MemberInfo[]>("get_members");
}

export async function addReaction(messageId: number, emoji: string): Promise<void> {
  return invoke<void>("add_reaction", { messageId, emoji });
}

export async function removeReaction(messageId: number, emoji: string): Promise<void> {
  return invoke<void>("remove_reaction", { messageId, emoji });
}

export async function createThread(messageId: number, name?: string): Promise<void> {
  return invoke<void>("create_thread", { messageId, name: name ?? null });
}

export async function searchMessages(
  query: string,
  channelId?: number,
  limit?: number,
): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("search_messages", {
    query,
    channelId: channelId ?? null,
    limit: limit ?? null,
  });
}

export interface InviteResult {
  code: string;
  link: string;
  deep_link: string;
}

export async function createInvite(maxUses?: number): Promise<InviteResult> {
  return invoke<InviteResult>("create_invite", { maxUses: maxUses ?? null });
}

export async function createChannel(
  name: string,
  channelType: string,
  categoryId?: number,
): Promise<void> {
  return invoke<void>("create_channel", {
    name,
    channelType,
    categoryId: categoryId ?? null,
  });
}

export async function createCategory(name: string): Promise<void> {
  return invoke<void>("create_category", { name });
}

export async function deleteChannel(channelId: number): Promise<void> {
  return invoke<void>("delete_channel", { channelId });
}

export async function deleteCategory(categoryId: number): Promise<void> {
  return invoke<void>("delete_category", { categoryId });
}

export async function updateChannel(channelId: number, opts: {
  name?: string;
  topic?: string;
  nsfw?: boolean;
  slowModeSecs?: number;
  categoryId?: number | null;
  position?: number;
}): Promise<void> {
  const setCategory = opts.categoryId !== undefined;
  return invoke<void>("update_channel", {
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

export async function updateCategory(categoryId: number, opts: { name?: string; position?: number }): Promise<void> {
  return invoke<void>("update_category", { categoryId, ...opts });
}

export async function setChannelOverride(channelId: number, roleId: number, allow: number, deny: number): Promise<void> {
  return invoke<void>("set_channel_override", { channelId, roleId, allow, deny });
}

export async function pickFile(): Promise<string | null> {
  return invoke<string | null>("pick_file");
}

export async function uploadFile(channelId: number, filePath: string): Promise<number> {
  return invoke<number>("upload_file", { channelId, filePath });
}

export interface DownloadResult {
  data_url: string | null;
  file_name: string;
  mime_type: string;
  saved_path: string | null;
}

export async function downloadFile(fileId: number): Promise<DownloadResult> {
  return invoke<DownloadResult>("download_file", { fileId });
}

export async function fetchUrl(url: string, channelId: number): Promise<number> {
  return invoke<number>("fetch_url", { url, channelId });
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

export async function addFavorite(fileId: number, originalUrl?: string): Promise<FavoriteEntry> {
  return invoke<FavoriteEntry>("add_favorite", { fileId, originalUrl: originalUrl ?? null });
}

export async function listFavorites(): Promise<FavoriteEntry[]> {
  return invoke<FavoriteEntry[]>("list_favorites");
}

export async function removeFavorite(id: string): Promise<void> {
  return invoke<void>("remove_favorite", { id });
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

export async function openDm(targetKey: string): Promise<{ channel: ChannelInfo; participant: MemberInfo }> {
  return invoke("open_dm", { targetKey });
}

export async function listDms(): Promise<DmEntry[]> {
  return invoke<DmEntry[]>("list_dms");
}

export async function blockUser(targetKey: string): Promise<void> {
  return invoke("block_user", { targetKey });
}

export async function unblockUser(targetKey: string): Promise<void> {
  return invoke("unblock_user", { targetKey });
}
