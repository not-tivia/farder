import { invoke } from "@tauri-apps/api/core";
import type { ConnectResult, SendMessageResult, MessageInfo, MemberInfo } from "./types";

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
