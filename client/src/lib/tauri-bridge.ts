import { invoke } from "@tauri-apps/api/core";
import type { ConnectResult, SendMessageResult, MessageInfo, MessageInfoV2, ServerInfoV2, MemberInfo, ChannelInfo, CategoryInfo, RoleInfo, DmEntry, BannedMember, BotAlertInfo, WebhookInfo, WebhookTokenResult, CommandInfo, PollInfo, GiveawayInfo, EventInfo, EventState, ReminderInfo } from "./types";
import type { EmbedOutcome } from "./linkEmbed";

export interface UploadOutcome {
  fileId: number;
  contentHash: string;
  declaredType: string;
  size: number;
}

export interface AttachmentCapInput {
  contentHash: string;
  declaredType: string;
  size: number;
}

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

export async function restartLocalServers(): Promise<{ id: string; name: string }[]> {
  return invoke<{ id: string; name: string }[]>("restart_local_servers");
}

// ── Identity (no serverId) ────────────────────────────────────────────────────

export async function getPublicKey(): Promise<string | null> {
  return invoke<string | null>("get_public_key");
}

export type IdentityStatus = "none" | "plaintext" | "encrypted";

export interface CreateIdentityResult {
  public_key: string;
  recovery_phrase: string;
}

export async function identityStatus(): Promise<IdentityStatus> {
  return invoke<IdentityStatus>("identity_status");
}

export async function createIdentity(pin: string): Promise<CreateIdentityResult> {
  return invoke<CreateIdentityResult>("create_identity", { pin });
}

export async function unlockIdentity(pin: string): Promise<string> {
  return invoke<string>("unlock_identity", { pin });
}

export async function migratePlaintextIdentity(pin: string): Promise<CreateIdentityResult> {
  return invoke<CreateIdentityResult>("migrate_plaintext_identity", { pin });
}

export async function restoreIdentity(phrase: string, pin: string): Promise<string> {
  return invoke<string>("restore_identity", { phrase, pin });
}

// Save the recovery phrase as a PNG (rendered on a canvas, passed as raw
// base64) via a native Save dialog. Resolves true if saved, false if cancelled.
export async function saveRecoveryImage(pngBase64: string): Promise<boolean> {
  return invoke<boolean>("save_recovery_image", { pngBase64 });
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

export async function setLastServer(address: string): Promise<void> {
  return invoke<void>("save_last_server", { address });
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

export interface MemberProfileView {
  avatar_data_url: string | null;
  status: string | null;
}

export async function getMemberProfile(serverId: string, publicKey: string, profileHash: string): Promise<MemberProfileView | null> {
  return invoke<MemberProfileView | null>("get_member_profile", { serverId, publicKey, profileHash });
}

export async function getProfileStatus(): Promise<string | null> {
  return invoke<string | null>("get_profile_status");
}

export async function setProfileStatus(status: string | null): Promise<void> {
  return invoke<void>("set_profile_status", { status });
}

export async function setServerAvatarOverride(serverId: string, filePath: string): Promise<string> {
  return invoke<string>("set_server_avatar_override", { serverId, filePath });
}

export async function clearServerAvatarOverride(serverId: string): Promise<void> {
  return invoke<void>("clear_server_avatar_override", { serverId });
}

export async function getServerAvatarOverride(serverId: string): Promise<string | null> {
  return invoke<string | null>("get_server_avatar_override", { serverId });
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

export async function submitEvent(
  serverId: string,    // connection key (address) — routes the request
  logServerId: string, // genesis hash — stamps the event + keys the device chain
  channelId: number,
  content: string,
  replyTo?: string | null,
  attachments: AttachmentCapInput[] = [],
): Promise<{ event_hash: string; timestamp: number }> {
  return invoke("submit_event", { serverId, logServerId, channelId, content, replyTo: replyTo ?? null, attachments });
}

export async function redactAttachment(serverId: string, logServerId: string, contentHash: string): Promise<{ event_hash: string; timestamp: number }> {
  return invoke("redact_attachment", { serverId, logServerId, contentHash });
}

export async function fetchHistory(serverId: string, channelId: number, beforeId?: number, limit?: number): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("fetch_history", { serverId, channelId, beforeId: beforeId ?? null, limit: limit ?? null });
}

export async function fetchHistoryV2(serverId: string, channelId: number, beforeId?: number, limit?: number): Promise<MessageInfoV2[]> {
  return invoke<MessageInfoV2[]>("fetch_history_v2", { serverId, channelId, beforeId: beforeId ?? null, limit: limit ?? null });
}

export async function subscribeChannels(serverId: string, channelIds: number[]): Promise<void> {
  return invoke<void>("subscribe_channels", { serverId, channelIds });
}

export async function getServerInfo(serverId: string): Promise<ConnectResult> {
  return invoke<ConnectResult>("get_server_info", { serverId });
}

export async function getServerInfoV2(serverId: string): Promise<ServerInfoV2> {
  return invoke<ServerInfoV2>("get_server_info_v2", { serverId });
}

export async function getMembers(serverId: string): Promise<MemberInfo[]> {
  return invoke<MemberInfo[]>("get_members", { serverId });
}

export async function addReaction(serverId: string, messageId: number, emoji: string, fileId?: number): Promise<void> {
  return invoke<void>("add_reaction", { serverId, messageId, emoji, fileId: fileId ?? null });
}

export async function removeReaction(serverId: string, messageId: number, emoji: string, fileId?: number): Promise<void> {
  return invoke<void>("remove_reaction", { serverId, messageId, emoji, fileId: fileId ?? null });
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

export async function createE2eeChannel(serverId: string, logServerId: string, name: string): Promise<{ channel_id: number; class: string; event_hash: string }> {
  return invoke("create_e2ee_channel", { serverId, logServerId, name });
}

export async function addMembersToE2eeChannel(serverId: string, logServerId: string, channelId: number): Promise<{ added: Array<{ identity: string; device: string; epoch: number }>; skipped: Array<{ identity: string; device: string | null; reason: string }> }> {
  return invoke("add_members_to_e2ee_channel", { serverId, logServerId, channelId });
}

export async function publishOwnKeyPackage(serverId: string, logServerId: string, channelId: number): Promise<{ channel_id: number; event_hash: string }> {
  return invoke("publish_own_key_package", { serverId, logServerId, channelId });
}

export async function processMlsControlEvents(serverId: string, logServerId: string, channelId: number): Promise<{ channel_id: number; outcome: string; reason: string | null; processed: number; epoch: number; generation: number; confirmed: boolean; cursor: number }> {
  return invoke("process_mls_control_events", { serverId, logServerId, channelId });
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

export async function createInvite(serverId: string, logServerId: string | null, maxUses?: number, requiresApproval?: boolean): Promise<InviteResult> {
  return invoke<InviteResult>("create_invite", { serverId, logServerId, maxUses: maxUses ?? null, requiresApproval: requiresApproval ?? false });
}

export async function fetchUrl(serverId: string, url: string, channelId: number): Promise<number> {
  return invoke<number>("fetch_url", { serverId, url, channelId });
}

export async function uploadFile(serverId: string, channelId: number, filePath: string): Promise<UploadOutcome> {
  return invoke<UploadOutcome>("upload_file", { serverId, channelId, filePath });
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

export async function addBot(serverId: string, coinId: string, label: string): Promise<void> {
  return invoke("add_bot", { serverId, coinId, label });
}

export async function addCustomBot(serverId: string, name: string, sourceUrl: string, valuePath: string, unit: string | null): Promise<void> {
  return invoke("add_custom_bot", { serverId, name, sourceUrl, valuePath, unit: unit ?? null });
}

export async function removeBot(serverId: string, botPublicKey: string): Promise<void> {
  return invoke("remove_bot", { serverId, botPublicKey });
}

export async function getBotPollInterval(serverId: string): Promise<number> {
  return invoke("get_bot_poll_interval", { serverId });
}
export async function setBotPollInterval(serverId: string, secs: number): Promise<void> {
  return invoke("set_bot_poll_interval", { serverId, secs });
}

export async function addBotAlert(serverId: string, botPublicKey: string, metric: string, comparator: string, threshold: number): Promise<void> {
  return invoke("add_bot_alert", { serverId, botPublicKey, metric, comparator, threshold });
}

export async function removeBotAlert(serverId: string, alertId: number): Promise<void> {
  return invoke("remove_bot_alert", { serverId, alertId });
}

export async function listBotAlerts(serverId: string, botPublicKey: string): Promise<BotAlertInfo[]> {
  return invoke<BotAlertInfo[]>("list_bot_alerts", { serverId, botPublicKey });
}

export async function subscribeBot(serverId: string, botPublicKey: string): Promise<void> {
  return invoke("subscribe_bot", { serverId, botPublicKey });
}

export async function unsubscribeBot(serverId: string, botPublicKey: string): Promise<void> {
  return invoke("unsubscribe_bot", { serverId, botPublicKey });
}

export async function listMySubscriptions(serverId: string): Promise<string[]> {
  return invoke<string[]>("list_my_subscriptions", { serverId });
}

export async function updateRole(serverId: string, roleId: number, patch: { name?: string; permissions?: number; color?: string; position?: number; hoist?: boolean }): Promise<void> {
  return invoke<void>("update_role", { serverId, roleId, name: patch.name ?? null, permissions: patch.permissions ?? null, color: patch.color ?? null, position: patch.position ?? null, hoist: patch.hoist ?? null });
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

/** Starts a recording and returns its session id (pass it to stopRecording). */
export async function startRecording(): Promise<number> {
  return invoke<number>("start_recording");
}

/**
 * Stops a recording and returns the WAV path. With a session id, only that
 * recording is stopped (a stale id is rejected without touching a newer
 * recording). Without one, stops whatever is recording (wedge recovery).
 */
export async function stopRecording(session?: number): Promise<string> {
  return invoke<string>("stop_recording", { session: session ?? null });
}

export async function playAudioFile(path: string): Promise<void> {
  return invoke<void>("play_audio_file", { path });
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

// ── Voice pipeline (sub-project #3.3) ────────────────────────────────────────
//
// Distinct from the older `joinVoice` / `getVoiceState` helpers above, which
// only manipulate channel-membership state on the server. The `voice*`
// commands below drive the local audio pipeline (`VoiceController`): they
// open the QUIC stream session, derive + wrap the per-call stream key,
// spawn the send / mixer / recv tasks, and toggle mute/deafen on the
// local audio path.

export interface VoicePeer {
  pubkey: { bytes: number[] };
  speaking: boolean;
  muted: boolean;
  deafened: boolean;
}

export interface VoiceState {
  /// Present iff a call is active. 16-byte channel identifier serialized as
  /// a Vec<u8> (raw bytes), matching the controller's `ChannelId` type.
  channel_id: number[] | null;
  muted: boolean;
  deafened: boolean;
  transmitting: boolean;
  peers: VoicePeer[];
}

export interface VoiceLocalSpeakingPayload {
  speaking: boolean;
}

export interface VoicePeerSpeakingPayload {
  session_id: number[];
  pubkey: string;
  active: boolean;
}

export async function voiceJoin(serverId: string, channelId: number): Promise<void> {
  return invoke("voice_join", { serverId, channelId });
}

export async function voiceLeave(): Promise<void> {
  return invoke("voice_leave");
}

export type DisplaySourceKind = "Screen" | "Window";
export interface DisplaySource { id: string; kind: DisplaySourceKind; label: string; width: number; height: number; }
export async function listDisplaySources(): Promise<DisplaySource[]> {
  return invoke<DisplaySource[]>("list_display_sources");
}

export async function voiceStartScreenShare(fps: number, maxWidth: number, maxHeight: number, sourceId: string | null, audioDeviceId: string | null): Promise<void> {
  return invoke<void>("voice_start_screen_share", { fps, maxWidth, maxHeight, sourceId, audioDeviceId });
}

export async function voiceStopScreenShare(): Promise<void> {
  return invoke<void>("voice_stop_screen_share");
}

export async function voiceRequestKeyframe(): Promise<void> {
  return invoke<void>("voice_request_keyframe");
}

export async function voiceSetMute(muted: boolean): Promise<void> {
  return invoke("voice_set_mute", { muted });
}

export async function voiceSetDeafen(deafened: boolean): Promise<void> {
  return invoke("voice_set_deafen", { deafened });
}

export async function voiceGetState(): Promise<VoiceState> {
  return invoke("voice_get_state");
}

export interface ConnectionQualityPayload {
  rtt_ms: number;
  loss_pct: number;
}

export async function voiceToggleTransmit(): Promise<boolean> {
  return invoke<boolean>("voice_toggle_transmit");
}

export async function voiceSetPeerVolume(pubkeyHex: string, volume: number): Promise<void> {
  return invoke<void>("voice_set_peer_volume", { pubkeyHex, volume });
}

export async function voiceSetScreenAudioGain(pubkeyHex: string, gain: number): Promise<void> {
  return invoke<void>("voice_set_screen_audio_gain", { pubkeyHex, gain });
}

export async function getVoiceMode(): Promise<string> {
  return invoke<string>("get_voice_mode");
}

export async function setVoiceMode(mode: string): Promise<void> {
  return invoke<void>("set_voice_mode", { mode });
}

export async function getPttKey(): Promise<string> {
  return invoke<string>("get_ptt_key");
}

export async function setPttKey(key: string): Promise<void> {
  return invoke<void>("set_ptt_key", { key });
}

export async function getPeerVolumes(): Promise<Record<string, number>> {
  return invoke<Record<string, number>>("get_peer_volumes");
}

export async function getVoiceSensitivity(): Promise<number> {
  return invoke<number>("get_voice_sensitivity");
}

export async function setVoiceSensitivity(value: number): Promise<void> {
  return invoke<void>("set_voice_sensitivity", { value });
}

// ── Audio device selection ─────────────────────────────────────────────────

export interface AudioDeviceInfo {
  name: string;
  is_default: boolean;
}

export async function listInputDevices(): Promise<AudioDeviceInfo[]> {
  return invoke<AudioDeviceInfo[]>("list_input_devices");
}

export async function listOutputDevices(): Promise<AudioDeviceInfo[]> {
  return invoke<AudioDeviceInfo[]>("list_output_devices");
}

export async function getInputDevice(): Promise<string | null> {
  return invoke<string | null>("get_input_device");
}

export async function setInputDevice(name: string | null): Promise<void> {
  return invoke<void>("set_input_device", { name });
}

export async function getOutputDevice(): Promise<string | null> {
  return invoke<string | null>("get_output_device");
}

export async function setOutputDevice(name: string | null): Promise<void> {
  return invoke<void>("set_output_device", { name });
}

// Raw mic input level (RMS), emitted ~10x/sec while a voice call is active.
export interface VoiceInputLevelPayload {
  level: number;
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
  relayMode: "farder" | "selfhost" | "direct" = "farder",
  relayAddr?: string,
  relayFp?: string,
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
    relayMode,
    relayAddr: relayAddr ?? null,
    relayFp: relayFp ?? null,
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

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

export interface ThemeMeta {
  id: string;
  name: string;
  author: string;
  description: string;
  source: "builtin" | "user";
}

export interface ActiveTheme {
  id: string;
  css: string;
}

export async function listThemes(): Promise<ThemeMeta[]> {
  return invoke<ThemeMeta[]>("list_themes");
}

export async function loadThemeCss(id: string): Promise<string> {
  return invoke<string>("load_theme_css", { id });
}

export async function getActiveTheme(): Promise<ActiveTheme> {
  return invoke<ActiveTheme>("get_active_theme");
}

export async function setActiveTheme(id: string): Promise<void> {
  return invoke<void>("set_active_theme", { id });
}

export async function openThemesFolder(): Promise<void> {
  return invoke<void>("open_themes_folder");
}

export async function forkTheme(baseId: string, newId: string, name: string): Promise<string> {
  return invoke<string>("fork_theme", { baseId, newId, name });
}

export async function saveUserTheme(id: string, css: string): Promise<void> {
  return invoke<void>("save_user_theme", { id, css });
}

export async function addThemeAsset(themeId: string, sourcePath: string, targetFilename: string): Promise<string> {
  return invoke<string>("add_theme_asset", { themeId, sourcePath, targetFilename });
}

export async function deleteUserTheme(id: string): Promise<void> {
  return invoke<void>("delete_user_theme", { id });
}

export async function renameUserTheme(id: string, newName: string): Promise<void> {
  return invoke<void>("rename_user_theme", { id, newName });
}

export async function getThemeOrder(): Promise<string[]> {
  return invoke<string[]>("get_theme_order");
}

export async function setThemeOrder(ids: string[]): Promise<void> {
  return invoke<void>("set_theme_order", { ids });
}

export async function kickMember(serverId: string, memberKey: string, logServerId: string | null): Promise<void> {
  return invoke<void>("kick_member", { serverId, memberKey, logServerId });
}

export async function banMember(serverId: string, memberKey: string, logServerId: string | null, reason?: string): Promise<void> {
  return invoke<void>("ban_member", { serverId, memberKey, logServerId, reason: reason ?? null });
}

export async function unbanMember(serverId: string, memberKey: string, logServerId: string | null): Promise<void> {
  return invoke<void>("unban_member", { serverId, memberKey, logServerId });
}

export async function listBanned(serverId: string): Promise<BannedMember[]> {
  return invoke<BannedMember[]>("list_banned", { serverId });
}

export interface AuditEvent {
  id: number;
  actor: { bytes: number[] };
  target: { bytes: number[] } | null;
  action: string;
  metadata: Record<string, unknown>;
  timestamp_ms: number;
}

export async function timeoutMember(
  serverId: string,
  memberKey: string,
  untilMs: number,
  reason: string | null,
): Promise<void> {
  return invoke<void>("timeout_member", { serverId, memberKey, untilMs, reason });
}

export async function removeTimeout(serverId: string, memberKey: string): Promise<void> {
  return invoke<void>("remove_timeout", { serverId, memberKey });
}

export async function listAuditEvents(
  serverId: string,
  beforeId: number | null,
  limit: number,
): Promise<AuditEvent[]> {
  return invoke<AuditEvent[]>("list_audit_events", { serverId, beforeId, limit });
}

// ── Invite previews ───────────────────────────────────────────────────────────

export interface InvitePreviewResult {
  status: "ok" | "invalid" | "unavailable" | "none";
  server_name: string | null;
  member_count: number | null;
  online_count: number | null;
}

export async function getInvitePreview(link: string): Promise<InvitePreviewResult> {
  return invoke<InvitePreviewResult>("get_invite_preview", { link });
}

// ── Screenshare preview (Phase B loopback) ────────────────────────────────────

export async function startScreensharePreview(fps: number, maxWidth: number, maxHeight: number): Promise<void> {
  return invoke<void>("start_screenshare_preview", { fps, maxWidth, maxHeight });
}

export async function stopScreensharePreview(): Promise<void> {
  return invoke<void>("stop_screenshare_preview");
}

// ── Screen-audio output device enumeration (Phase D Task 7) ──────────────────

export interface AudioOutputDevice { id: string; name: string; is_default: boolean; }

export async function listAudioOutputDevices(): Promise<AudioOutputDevice[]> {
  return invoke<AudioOutputDevice[]>("list_audio_output_devices");
}

// ── Rich link embeds (Phase 6) ────────────────────────────────────────────────

export function getLinkEmbed(url: string): Promise<EmbedOutcome> {
  return invoke<EmbedOutcome>("get_link_embed", { url });
}

export function getProxiedMedia(url: string): Promise<{ content_type: string; data_base64: string }> {
  return invoke<{ content_type: string; data_base64: string }>("get_proxied_media", { url });
}

// ── Data-saver embed toggle ───────────────────────────────────────────────────

export function getDataSaverEmbeds(): Promise<boolean> {
  return invoke<boolean>("get_data_saver_embeds");
}

export function setDataSaverEmbeds(enabled: boolean): Promise<void> {
  return invoke<void>("set_data_saver_embeds", { enabled });
}

// ── Presence settings ─────────────────────────────────────────────────────────

export const getPresenceEnabled = () => invoke<boolean>("get_presence_enabled");
export const setPresenceEnabled = (enabled: boolean) => invoke<void>("set_presence_enabled", { enabled });
export const getPresenceMusic = () => invoke<boolean>("get_presence_music");
export const setPresenceMusic = (enabled: boolean) => invoke<void>("set_presence_music", { enabled });

// ── Mesh log membership ───────────────────────────────────────────────────────

export async function joinLogServer(serverId: string, logServerId: string, inviteCode: string): Promise<void> {
  return invoke("join_log_server", { serverId, logServerId, inviteCode });
}

/** Emit a signed `MemberApproved` event for the target member. Requires approver identity to be unlocked. */
export async function approveMember(serverId: string, logServerId: string, member: string): Promise<void> {
  return invoke("approve_member", { serverId, logServerId, member });
}

/** Emit a signed `MemberRemoved` event for the target member (deny approval or remove). Requires approver identity to be unlocked. */
export async function denyMember(serverId: string, logServerId: string, member: string): Promise<void> {
  return invoke("deny_member", { serverId, logServerId, member });
}

/** Return the caller's membership status on a server: `"member"` | `"pending"` | `"none"`. */
export async function getMembershipStatus(serverId: string): Promise<string> {
  return invoke("get_membership_status", { serverId });
}

/** Return the list of members currently awaiting approval on this server (approver-gated). */
export async function getPendingMembers(serverId: string): Promise<MemberInfo[]> {
  return invoke("get_pending_members", { serverId });
}

// ── Webhook management (MANAGE_SERVER gated) ──────────────────────────────────

/** Create an incoming webhook for a channel. Returns id, token, and server_id_hex
 *  (relay server hex for URL building). Token is shown once; never retrievable. */
export async function createWebhook(serverId: string, channelId: number, name: string): Promise<WebhookTokenResult> {
  return invoke<WebhookTokenResult>("create_webhook", { serverId, channelId, name });
}

/** List all webhooks for a channel (no tokens returned). */
export async function listWebhooks(serverId: string, channelId: number): Promise<WebhookInfo[]> {
  return invoke<WebhookInfo[]>("list_webhooks", { serverId, channelId });
}

/** Delete a webhook by id. */
export async function deleteWebhook(serverId: string, id: number): Promise<void> {
  return invoke<void>("delete_webhook", { serverId, id });
}

/** Rotate the secret token for a webhook. Returns id, new token, and server_id_hex.
 *  Token is shown once; never retrievable. */
export async function regenerateWebhookToken(serverId: string, id: number): Promise<WebhookTokenResult> {
  return invoke<WebhookTokenResult>("regenerate_webhook_token", { serverId, id });
}

// ── Slash command management ──────────────────────────────────────────────────

/** List all slash commands registered on a server. Available to all members. */
export async function listCommands(serverId: string): Promise<CommandInfo[]> {
  return invoke<CommandInfo[]>("list_commands", { serverId });
}

/** Register a new slash command (MANAGE_SERVER gated). Kind is "text" or "api";
 *  the remaining fields are kind-specific and optional. */
export async function addCommand(
  serverId: string,
  name: string,
  trigger: string,
  description: string,
  kind: string,
  bodyText?: string | null,
  urlTemplate?: string | null,
  valuePath?: string | null,
  responseTemplate?: string | null,
  unit?: string | null,
): Promise<void> {
  return invoke<void>("add_command", {
    serverId,
    name,
    trigger,
    description,
    kind,
    bodyText: bodyText ?? null,
    urlTemplate: urlTemplate ?? null,
    valuePath: valuePath ?? null,
    responseTemplate: responseTemplate ?? null,
    unit: unit ?? null,
  });
}

/** Delete a slash command by id (MANAGE_SERVER gated). */
export async function deleteCommand(serverId: string, id: number): Promise<void> {
  return invoke<void>("delete_command", { serverId, id });
}

/** Invoke a slash command. Throws with the server's reason string if the
 *  command fails (unknown trigger, rate-limit, etc.) so the caller can show
 *  it locally without posting anything to the channel.
 *
 *  Resolves to a NOTICE string when the server answered with an invoker-only
 *  confirmation that posted nothing (the `/remind` kind), else `null`. Callers
 *  that post to the channel can ignore the value. */
export async function runCommand(serverId: string, trigger: string, channelId: number, args: string): Promise<string | null> {
  return invoke<string | null>("run_command", { serverId, trigger, channelId, args });
}

// ── Poll & giveaway widgets ───────────────────────────────────────────────────

/** Full poll state: shared PollInfo plus my own vote (self-only). */
export interface PollState {
  poll: PollInfo;
  my_vote: number | null;
}

/** Fetch a poll's current state plus my own vote — the widget's state-recovery
 *  path on mount/reconnect/server switch. */
export async function getPoll(serverId: string, pollId: number): Promise<PollState> {
  return invoke<PollState>("get_poll", { serverId, pollId });
}

/** Cast (or change) my vote on a poll option. Counts arrive via server:poll_updated. */
export async function votePoll(serverId: string, pollId: number, optionIndex: number): Promise<void> {
  return invoke<void>("vote_poll", { serverId, pollId, optionIndex });
}

/** Retract my vote on an open poll. */
export async function retractVote(serverId: string, pollId: number): Promise<void> {
  return invoke<void>("retract_vote", { serverId, pollId });
}

/** Close a poll early (creator or MANAGE_SERVER — enforced server-side). */
export async function closePoll(serverId: string, pollId: number): Promise<void> {
  return invoke<void>("close_poll", { serverId, pollId });
}

/** Full giveaway state: shared GiveawayInfo plus whether I entered (self-only). */
export interface GiveawayState {
  giveaway: GiveawayInfo;
  my_entered: boolean;
}

/** Fetch a giveaway's current state plus whether I entered — the widget's
 *  state-recovery path on mount/reconnect/server switch. */
export async function getGiveaway(serverId: string, giveawayId: number): Promise<GiveawayState> {
  return invoke<GiveawayState>("get_giveaway", { serverId, giveawayId });
}

/** Enter an open giveaway (idempotent — already-entered is Ok). */
export async function enterGiveaway(serverId: string, giveawayId: number): Promise<void> {
  return invoke<void>("enter_giveaway", { serverId, giveawayId });
}

/** Leave an open giveaway (idempotent). */
export async function leaveGiveaway(serverId: string, giveawayId: number): Promise<void> {
  return invoke<void>("leave_giveaway", { serverId, giveawayId });
}

/** Cancel an open giveaway (creator or MANAGE_SERVER — enforced server-side). */
export async function cancelGiveaway(serverId: string, giveawayId: number): Promise<void> {
  return invoke<void>("cancel_giveaway", { serverId, giveawayId });
}

/** Redraw a finished giveaway's winner (creator or MANAGE_SERVER — enforced server-side). */
export async function rerollGiveaway(serverId: string, giveawayId: number): Promise<void> {
  return invoke<void>("reroll_giveaway", { serverId, giveawayId });
}

// ── Event widgets ────────────────────────────────────────────────────────────

/** Fetch an event's current state plus my own RSVP — the widget's
 *  state-recovery path on mount/reconnect/server switch. Missing/invisible
 *  events reject with the server's opaque "event not found". */
export async function getEvent(serverId: string, eventId: number): Promise<EventState> {
  return invoke<EventState>("get_event", { serverId, eventId });
}

/** Set (or change) my RSVP — "going" | "maybe" | "no". The updated roster
 *  arrives via server:event_updated. */
export async function rsvpEvent(serverId: string, eventId: number, response: string): Promise<void> {
  return invoke<void>("rsvp_event", { serverId, eventId, response });
}

/** Withdraw my RSVP entirely (pressing my own option — the poll-retract idiom). */
export async function clearRsvp(serverId: string, eventId: number): Promise<void> {
  return invoke<void>("clear_rsvp", { serverId, eventId });
}

/** Cancel an upcoming event (creator or MANAGE_SERVER — enforced server-side).
 *  The server DMs every responder once. */
export async function cancelEvent(serverId: string, eventId: number): Promise<void> {
  return invoke<void>("cancel_event", { serverId, eventId });
}

/** Edit an upcoming event — FULL REPLACE of the editable fields (send them all
 *  every time; same validation as creation). Creator or MANAGE_SERVER. */
export async function editEvent(
  serverId: string,
  eventId: number,
  title: string,
  description: string | null,
  location: string | null,
  startsAt: number,
  remindLead: number | null,
): Promise<void> {
  return invoke<void>("edit_event", { serverId, eventId, title, description, location, startsAt, remindLead });
}

// ── Personal reminders ───────────────────────────────────────────────────────

/** My own pending reminders, soonest first. Owner-scoped server-side by the
 *  connection key — nothing here is another member's. */
export async function listMyReminders(serverId: string): Promise<ReminderInfo[]> {
  return invoke<ReminderInfo[]>("list_my_reminders", { serverId });
}

/** Cancel one of MY pending reminders. Someone else's id rejects with the same
 *  opaque "reminder not found" as a nonexistent one. */
export async function cancelReminder(serverId: string, reminderId: number): Promise<void> {
  return invoke<void>("cancel_reminder", { serverId, reminderId });
}

/** The channel's open widgets for the active-widgets bar — each list oldest-first,
 *  20 combined server-side. Shared state only (no per-viewer my_vote/my_entered/
 *  my_rsvp; the chip dropdown's widget pulls those via its own getPoll/
 *  getGiveaway/getEvent mount fetch). Visibility failures reject with the
 *  server's opaque "channel not found". */
export async function listActiveWidgets(serverId: string, channelId: number): Promise<{ polls: PollInfo[]; giveaways: GiveawayInfo[]; events: EventInfo[] }> {
  return invoke<{ polls: PollInfo[]; giveaways: GiveawayInfo[]; events: EventInfo[] }>("list_active_widgets", { serverId, channelId });
}
