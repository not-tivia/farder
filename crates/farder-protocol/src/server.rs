use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

pub const DELETED_USER_KEY: [u8; 32] = [0u8; 32];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeletionStatus {
    pub pending: bool,
    pub requested_at: Option<u64>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelType {
    Text,
    Announcement,
    Thread,
    Dm,
    Voice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Audio,
    Video,
    ScreenAudio,
}

/// What a member is doing right now (ephemeral activity). Source-agnostic so a
/// future game source produces the same shape as the music source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresenceKind { Music, Game, Ticker }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub kind: PresenceKind,
    /// Primary line: music = track title; game = game name.
    pub details: String,
    /// Secondary line: music = artist; game = None (for now).
    pub state: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceMember {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DmEntry {
    pub channel: ChannelInfo,
    pub participant: MemberInfo,
    pub last_message: Option<MessageInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AttachmentInfo {
    pub id: u64,
    pub file_id: u64,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
    /// Hex SHA-256 of the bytes — lets the client build an AttachmentRedacted event.
    #[serde(default)]
    pub content_hash: String,
    /// Redaction state: None = live; Some(false) = removed by the uploader;
    /// Some(true) = removed by a moderator (redactor != original uploader).
    #[serde(default)]
    pub redacted_by_moderator: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReactionGroup {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
    #[serde(default)]
    pub file_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadRequest {
    pub channel_id: u64,
    pub file_name: String,
    pub file_size: u64,
    pub hash: String,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum UploadResponse {
    Ready,
    Complete { file_id: u64 },
    Error { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub file_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DownloadResponse {
    Start { file_name: String, file_size: u64, hash: String, mime_type: String },
    Error { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MessageInfo {
    pub id: u64,
    pub channel_id: u64,
    pub author: PublicKey,
    pub content: String,
    pub timestamp: u64,
    pub edited_at: Option<u64>,
    pub reply_to: Option<u64>,
    pub pinned: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub reactions: Vec<ReactionGroup>,
    pub thread_id: Option<u64>,
    pub thread_message_count: Option<u32>,
    /// Display-name override for webhook-posted messages (non-member author).
    /// None for all regular member messages.
    #[serde(default)]
    pub author_name_override: Option<String>,
    /// Visual badge shown next to the message author (e.g. "WEBHOOK", "BOT").
    /// None for all regular member messages.
    #[serde(default)]
    pub author_badge: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelInfo {
    pub id: u64,
    pub name: String,
    pub channel_type: ChannelType,
    pub category_id: Option<u64>,
    pub position: u32,
    pub topic: Option<String>,
    pub nsfw: bool,
    pub slow_mode_secs: u32,
    pub retention_secs: Option<u64>,
    pub thread_parent_message_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CategoryInfo {
    pub id: u64,
    pub name: String,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RoleInfo {
    pub id: u64,
    pub name: String,
    pub permissions: u64,
    pub color: Option<String>,
    pub position: u32,
    #[serde(default)]
    pub hoist: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MemberInfo {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
    pub role_ids: Vec<u64>,
    #[serde(default)]
    pub timeout_until: Option<u64>,
    #[serde(default)]
    pub timeout_reason: Option<String>,
    /// Hex-encoded SHA-256 of the member's canonical serialized SignedProfile.
    #[serde(default)]
    pub profile_hash: Option<String>,
    #[serde(default)]
    pub presence: Option<Presence>,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BannedMember {
    pub public_key: PublicKey,
    pub display_name: String,
    #[serde(default)]
    pub ban_reason: Option<String>,
    pub banned_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: u64,
    pub actor: PublicKey,
    #[serde(default)]
    pub target: Option<PublicKey>,
    pub action: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OverrideInfo {
    pub role_id: u64,
    pub allow: u64,
    pub deny: u64,
}

/// A price alert as returned to clients (read-only view; armed state is internal).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BotAlertInfo {
    pub id: i64,
    pub metric: String,
    pub comparator: String,
    pub threshold: f64,
}

/// A webhook summary returned by `ListWebhooks` (no token field — tokens are
/// write-only after creation/rotation).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookInfo {
    pub id: i64,
    pub channel_id: u64,
    pub name: String,
}

/// A slash-command summary returned by `ListCommands`. Deliberately omits
/// `url_template` and `body_text` — those fields may hold API keys and must
/// never be exposed to members via this response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    pub id: i64,
    pub trigger: String,
    pub description: String,
    pub takes_arg: bool,
}

/// First frame on every relay-bridged stream, identifying its role. Relay-mode
/// only; direct connections do not use it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RelayStreamRole {
    /// A new client session: the server runs the auth handshake on this stream.
    Primary,
    /// A file-transfer stream for an already-authenticated session, identified
    /// by the session token the server issued at login.
    Session { token: Vec<u8> },
    /// An incoming webhook delivery: the relay forwards the raw HTTP body and
    /// the webhook token to the server. The server validates, parses, and posts.
    Webhook { token: String, body: Vec<u8> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientFrame {
    Authenticate {
        public_key: PublicKey,
        signed_challenge: Vec<u8>,
        invite_code: Option<String>,
        setup_token: Option<String>,
    },
    Request {
        id: u32,
        body: ServerRequest,
    },
    /// Pre-auth invite preview: sent INSTEAD of Authenticate after the
    /// Challenge. Valid-code-gated; the connection is throwaway.
    GetInvitePreview { code: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerRequest {
    Subscribe { channel_ids: Vec<u64> },
    SendMessage { channel_id: u64, content: String, reply_to: Option<u64>, attachment_ids: Vec<u64> },
    EditMessage { message_id: u64, new_content: String },
    DeleteMessage { message_id: u64 },
    FetchHistory { channel_id: u64, before_id: Option<u64>, limit: u32 },
    PinMessage { message_id: u64 },
    UnpinMessage { message_id: u64 },
    Search { query: String, channel_id: Option<u64>, limit: u32 },
    Typing { channel_id: u64 },
    CreateChannel { name: String, channel_type: ChannelType, category_id: Option<u64>, position: Option<u32> },
    UpdateChannel { channel_id: u64, name: Option<String>, topic: Option<String>, nsfw: Option<bool>, slow_mode_secs: Option<u32>, retention_secs: Option<Option<u64>>, category_id: Option<Option<u64>>, position: Option<u32> },
    DeleteChannel { channel_id: u64 },
    CreateCategory { name: String, position: Option<u32> },
    UpdateCategory { category_id: u64, name: Option<String>, position: Option<u32> },
    DeleteCategory { category_id: u64 },
    CreateRole { name: String, permissions: u64, color: Option<String>, position: Option<u32>, #[serde(default)] hoist: Option<bool> },
    UpdateRole { role_id: u64, name: Option<String>, permissions: Option<u64>, color: Option<String>, position: Option<u32>, #[serde(default)] hoist: Option<bool> },
    DeleteRole { role_id: u64 },
    AssignRole { member_key: PublicKey, role_id: u64 },
    RemoveRole { member_key: PublicKey, role_id: u64 },
    KickMember { member_key: PublicKey },
    BanMember {
        member_key: PublicKey,
        #[serde(default)]
        reason: Option<String>,
    },
    UnbanMember {
        member_key: PublicKey,
    },
    TimeoutMember { member_key: PublicKey, until_ms: u64, reason: Option<String> },
    RemoveTimeout { member_key: PublicKey },
    ListAuditEvents { before_id: Option<u64>, limit: u32 },
    ListBanned,
    CreateInvite { max_uses: Option<u32>, expires_in_secs: Option<u64>, target_channel: Option<u64> },
    /// Resolve an invite code to the hash of its log `InviteCreated` event, so a
    /// joiner can cite it in a `MemberJoined`. Returns None for an unknown code.
    ResolveInvite { code: String },
    GetServerInfo,
    GetMembers,
    /// Store the sender's signed profile (serialized `farder_crypto::profile::SignedProfile`).
    UpdateProfile { profile: Vec<u8> },
    /// Set or clear the sender's ephemeral presence (None clears it).
    UpdatePresence { presence: Option<Presence> },
    /// Fetch a member's stored signed profile blob.
    GetMemberProfile { member_key: PublicKey },
    SetChannelOverride { channel_id: u64, role_id: u64, allow: u64, deny: u64 },
    SetCategoryOverride { category_id: u64, role_id: u64, allow: u64, deny: u64 },
    CreateThread { message_id: u64, name: Option<String> },
    AddReaction { message_id: u64, emoji: String, #[serde(default)] file_id: Option<u64> },
    RemoveReaction { message_id: u64, emoji: String, #[serde(default)] file_id: Option<u64> },
    RequestDeletion,
    CancelDeletion,
    GetDeletionStatus,
    FetchUrl { url: String, channel_id: u64 },
    OpenDm { target_key: PublicKey },
    ListDms,
    BlockUser { target_key: PublicKey },
    UnblockUser { target_key: PublicKey },
    JoinStream { channel_id: u64 },
    LeaveStream,
    EnableTrack { kind: TrackKind },
    DisableTrack { kind: TrackKind },
    SetDeafen { deafened: bool },
    SetMute { muted: bool },
    OfferStreamKey {
        kind: TrackKind,
        wrapped_keys: Vec<(PublicKey, Vec<u8>)>,
    },
    JoinChannelMedia { channel_id: u64 },
    LeaveChannelMedia { channel_id: u64 },
    GetMediaState { channel_id: u64 },
    /// Submit a signed mesh event (Rung 1). The server validates it through the
    /// authorization log and, for MessagePosted, derives a `messages` row.
    SubmitEvent { event: farder_crypto::event_log::Event },
    /// Ask the server whether the caller is a member, pending approval, or neither
    /// (per the event log). Allowed for non-members so a pending joiner can learn it.
    GetMembershipStatus,
    /// List members currently awaiting approval (pending-approval joins). Gated to
    /// holders of KICK_MEMBERS (i.e. approvers) and the owner.
    GetPendingMembers,
    /// Register a new server-managed crypto-ticker bot (owner only).
    AddBot { coin_id: String, label: String },
    /// Remove a server-managed bot by its public key (owner only).
    RemoveBot { bot_public_key: PublicKey },
    /// Set the bot price poll interval in seconds (MANAGE_SERVER gated).
    SetBotPollInterval { secs: u64 },
    /// Query the current bot price poll interval in seconds.
    GetBotPollInterval,
    /// Add a price alert for a bot (MANAGE_SERVER gated).
    AddBotAlert { bot_public_key: PublicKey, metric: String, comparator: String, threshold: f64 },
    /// Remove a price alert by id (MANAGE_SERVER gated).
    RemoveBotAlert { alert_id: i64 },
    /// List all price alerts for a bot (MANAGE_SERVER gated).
    ListBotAlerts { bot_public_key: PublicKey },
    /// Subscribe the authenticated member to a bot's alerts (any member).
    SubscribeBot { bot_public_key: PublicKey },
    /// Unsubscribe the authenticated member from a bot's alerts (any member).
    UnsubscribeBot { bot_public_key: PublicKey },
    /// List all bots the authenticated member is subscribed to (any member).
    ListMySubscriptions,
    /// Create an incoming webhook for a channel (MANAGE_SERVER gated).
    CreateWebhook { channel_id: u64, name: String },
    /// List webhooks for a channel (MANAGE_SERVER gated; tokens not returned).
    ListWebhooks { channel_id: u64 },
    /// Delete a webhook by id (MANAGE_SERVER gated).
    DeleteWebhook { id: i64 },
    /// Rotate the secret token for a webhook (MANAGE_SERVER gated).
    RegenerateWebhookToken { id: i64 },
    /// Register a new custom-monitor bot that polls an arbitrary JSON API (MANAGE_SERVER gated).
    AddCustomBot { name: String, source_url: String, value_path: String, unit: Option<String> },
    /// List all slash commands (available to all members).
    ListCommands {},
    /// Create a new slash command (MANAGE_SERVER gated).
    AddCommand {
        name: String,
        trigger: String,
        description: String,
        kind: String,
        body_text: Option<String>,
        url_template: Option<String>,
        value_path: Option<String>,
        response_template: Option<String>,
        unit: Option<String>,
    },
    /// Delete a slash command by id (MANAGE_SERVER gated).
    DeleteCommand { id: i64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerFrame {
    Challenge { nonce: [u8; 32] },
    Authenticated { session_token: Vec<u8> },
    AuthError { reason: String },
    Response { request_id: u32, body: ServerResponse },
    Event(ServerEvent),
    InvitePreview { server_name: String, member_count: u32, online_count: u32 },
    /// Uniform for invalid/expired/exhausted codes — reveals nothing.
    InvitePreviewError { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerResponse {
    Ok,
    Error { reason: String },
    MessageSent { id: u64, timestamp: u64 },
    History { messages: Vec<MessageInfo> },
    SearchResults { messages: Vec<MessageInfo> },
    ServerInfo {
        name: String,
        member_count: u32,
        channels: Vec<ChannelInfo>,
        categories: Vec<CategoryInfo>,
        roles: Vec<RoleInfo>,
        #[serde(default)]
        owner_public_key: Option<PublicKey>,
        #[serde(default)]
        server_id: Option<String>,
    },
    Members { members: Vec<MemberInfo> },
    MemberProfile { member_key: PublicKey, #[serde(default)] profile: Option<Vec<u8>> },
    BannedMembers {
        entries: Vec<BannedMember>,
    },
    AuditEventsList { events: Vec<AuditEvent> },
    InviteCreated { code: String },
    InviteResolved { invite_event: Option<String> },
    DeletionStatusResp { status: DeletionStatus },
    UrlFetched { file_id: u64 },
    DmOpened { channel: ChannelInfo, participant: MemberInfo },
    DmList { dms: Vec<DmEntry> },
    StreamSessionStarted { session_id: [u8; 16] },
    MediaStateResp { participants: Vec<VoiceMember> },
    EventAccepted { event_hash: String, timestamp: u64 },
    MembershipStatus { status: String },
    PendingMembers { members: Vec<MemberInfo> },
    BotPollInterval { secs: u64 },
    /// The list of price alerts for a bot.
    BotAlerts { alerts: Vec<BotAlertInfo> },
    /// The list of bot public keys the authenticated member is subscribed to.
    MySubscriptions { bot_public_keys: Vec<PublicKey> },
    /// The id and secret token for a newly created or rotated webhook.
    /// `server_id_hex` is present on relay-enabled servers so the client can
    /// assemble the delivery URL; `None` on legacy direct-only servers.
    WebhookToken { id: i64, token: String, server_id_hex: Option<String> },
    /// The webhooks registered for a channel (no tokens).
    Webhooks { webhooks: Vec<WebhookInfo> },
    /// The slash commands registered for the server (safe fields only — no secrets).
    Commands { commands: Vec<CommandInfo> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerEvent {
    NewMessage { message: MessageInfo },
    MessageEdited { message_id: u64, channel_id: u64, new_content: String, edited_at: u64 },
    MessageDeleted { message_id: u64, channel_id: u64 },
    MessagePinned { message_id: u64, channel_id: u64 },
    MessageUnpinned { message_id: u64, channel_id: u64 },
    MemberJoined { public_key: PublicKey, display_name: String },
    MemberLeft { public_key: PublicKey },
    MemberBanned {
        public_key: PublicKey,
        #[serde(default)]
        reason: Option<String>,
    },
    MemberUnbanned {
        public_key: PublicKey,
    },
    MemberTimeoutChanged {
        public_key: PublicKey,
        #[serde(default)]
        until_ms: Option<u64>,
        #[serde(default)]
        reason: Option<String>,
    },
    MemberProfileUpdated {
        public_key: PublicKey,
        /// Hex-encoded SHA-256 of the member's canonical serialized SignedProfile.
        #[serde(default)]
        profile_hash: Option<String>,
    },
    /// A member's ephemeral presence changed (None = cleared/offline).
    MemberPresenceUpdated { public_key: PublicKey, presence: Option<Presence> },
    YouWereKicked,
    YouWereBanned {
        #[serde(default)]
        reason: Option<String>,
    },
    AuditEventCreated { event: AuditEvent },
    TypingStarted { channel_id: u64, public_key: PublicKey },
    ChannelCreated { channel: ChannelInfo },
    ChannelUpdated { channel: ChannelInfo },
    ChannelDeleted { channel_id: u64 },
    CategoryCreated { category: CategoryInfo },
    CategoryUpdated { category: CategoryInfo },
    CategoryDeleted { category_id: u64 },
    RoleCreated { role: RoleInfo },
    RoleUpdated { role: RoleInfo },
    RoleDeleted { role_id: u64 },
    PermissionsChanged,
    ReactionAdded { message_id: u64, channel_id: u64, emoji: String, public_key: PublicKey, #[serde(default)] file_id: Option<u64> },
    ReactionRemoved { message_id: u64, channel_id: u64, emoji: String, public_key: PublicKey, #[serde(default)] file_id: Option<u64> },
    DeletionRequested { public_key: PublicKey },
    DeletionCancelled { public_key: PublicKey },
    DeletionExecuted { public_key: PublicKey },
    DmCreated { channel: ChannelInfo, participant: MemberInfo },
    MediaJoined  { channel_id: u64, public_key: PublicKey, display_name: String },
    MediaLeft    { channel_id: u64, public_key: PublicKey },
    StreamJoined {
        channel_id: u64,
        public_key: PublicKey,
        display_name: String,
        session_id: [u8; 16],
        active_tracks: Vec<TrackKind>,
        muted: bool,
        deafened: bool,
    },
    StreamLeft {
        channel_id: u64,
        session_id: [u8; 16],
    },
    TrackEnabled  { channel_id: u64, session_id: [u8; 16], kind: TrackKind },
    TrackDisabled { channel_id: u64, session_id: [u8; 16], kind: TrackKind },
    TrackActivityChanged {
        channel_id: u64,
        session_id: [u8; 16],
        kind: TrackKind,
        active: bool,
    },
    StreamStateChanged {
        channel_id: u64,
        session_id: [u8; 16],
        muted: bool,
        deafened: bool,
    },
    StreamCallIncoming {
        channel_id: u64,
        caller: PublicKey,
        caller_name: String,
    },
    StreamCallEnded { channel_id: u64 },
    StreamKeyOffer {
        channel_id: u64,
        sender: PublicKey,
        session_id: [u8; 16],
        kind: TrackKind,
        wrapped_key: Vec<u8>,
    },
    /// A membership transition (join-pending / approve / remove / ban) for `public_key`.
    /// Clients re-fetch their own status + the member list + the pending queue on this.
    MembershipChanged { public_key: PublicKey },
    /// An attachment was taken down (bytes gone). Clients flip its placeholder.
    AttachmentRedacted { content_hash: String, by_moderator: bool },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;
    use farder_crypto::identity::Keypair;

    #[test]
    fn test_invite_preview_frames_roundtrip() {
        let f = ClientFrame::GetInvitePreview { code: "AbCd1234".to_string() };
        let bytes = codec::encode(&f).unwrap();
        match codec::decode::<ClientFrame>(&bytes).unwrap() {
            ClientFrame::GetInvitePreview { code } => assert_eq!(code, "AbCd1234"),
            other => panic!("wrong variant: {other:?}"),
        }

        let f = ServerFrame::InvitePreview { server_name: "The Spot".into(), member_count: 12, online_count: 3 };
        let bytes = codec::encode(&f).unwrap();
        match codec::decode::<ServerFrame>(&bytes).unwrap() {
            ServerFrame::InvitePreview { server_name, member_count, online_count } => {
                assert_eq!(server_name, "The Spot");
                assert_eq!(member_count, 12);
                assert_eq!(online_count, 3);
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let f = ServerFrame::InvitePreviewError { reason: "invalid".into() };
        let bytes = codec::encode(&f).unwrap();
        assert!(matches!(codec::decode::<ServerFrame>(&bytes).unwrap(), ServerFrame::InvitePreviewError { .. }));
    }

    #[test]
    fn test_roundtrip_client_frame_authenticate() {
        let kp = Keypair::generate();
        let frame = ClientFrame::Authenticate {
            public_key: kp.public_key(),
            signed_challenge: vec![1, 2, 3],
            invite_code: Some("abc123".to_string()),
            setup_token: None,
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Authenticate { public_key, invite_code, .. } => {
                assert_eq!(public_key, kp.public_key());
                assert_eq!(invite_code, Some("abc123".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_client_frame_request() {
        let frame = ClientFrame::Request {
            id: 42,
            body: ServerRequest::SendMessage {
                channel_id: 1,
                content: "hello".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { id, body } => {
                assert_eq!(id, 42);
                match body {
                    ServerRequest::SendMessage { channel_id, content, reply_to, attachment_ids } => {
                        assert_eq!(channel_id, 1);
                        assert_eq!(content, "hello");
                        assert!(reply_to.is_none());
                        assert!(attachment_ids.is_empty());
                    }
                    _ => panic!("wrong request variant"),
                }
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_frame_challenge() {
        let nonce = [42u8; 32];
        let frame = ServerFrame::Challenge { nonce };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Challenge { nonce: n } => assert_eq!(n, nonce),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_frame_response() {
        let kp = Keypair::generate();
        let msg = MessageInfo {
            id: 1,
            channel_id: 5,
            author: kp.public_key(),
            content: "test message".to_string(),
            timestamp: 1000,
            edited_at: None,
            reply_to: None,
            pinned: false,
            attachments: vec![],
            reactions: vec![],
            thread_id: None,
            thread_message_count: None,
            author_name_override: None,
            author_badge: None,
        };
        let frame = ServerFrame::Response {
            request_id: 7,
            body: ServerResponse::History { messages: vec![msg.clone()] },
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Response { request_id, body } => {
                assert_eq!(request_id, 7);
                match body {
                    ServerResponse::History { messages } => {
                        assert_eq!(messages.len(), 1);
                        assert_eq!(messages[0].content, "test message");
                    }
                    _ => panic!("wrong response variant"),
                }
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_event() {
        let kp = Keypair::generate();
        let event = ServerEvent::NewMessage {
            message: MessageInfo {
                id: 99,
                channel_id: 3,
                author: kp.public_key(),
                content: "event msg".to_string(),
                timestamp: 2000,
                edited_at: Some(2001),
                reply_to: Some(50),
                pinned: true,
                attachments: vec![],
                reactions: vec![],
                thread_id: None,
                thread_message_count: None,
                author_name_override: None,
                author_badge: None,
            },
        };
        let frame = ServerFrame::Event(event);
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Event(ServerEvent::NewMessage { message }) => {
                assert_eq!(message.id, 99);
                assert_eq!(message.edited_at, Some(2001));
                assert_eq!(message.reply_to, Some(50));
                assert!(message.pinned);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_all_request_variants() {
        let kp = Keypair::generate();
        let requests = vec![
            ServerRequest::Subscribe { channel_ids: vec![1, 2, 3] },
            ServerRequest::SendMessage { channel_id: 1, content: "hi".into(), reply_to: Some(5), attachment_ids: vec![] },
            ServerRequest::EditMessage { message_id: 10, new_content: "edited".into() },
            ServerRequest::DeleteMessage { message_id: 10 },
            ServerRequest::FetchHistory { channel_id: 1, before_id: Some(100), limit: 50 },
            ServerRequest::PinMessage { message_id: 10 },
            ServerRequest::UnpinMessage { message_id: 10 },
            ServerRequest::Search { query: "hello".into(), channel_id: Some(1), limit: 20 },
            ServerRequest::Typing { channel_id: 1 },
            ServerRequest::CreateChannel { name: "general".into(), channel_type: ChannelType::Text, category_id: None, position: Some(0) },
            ServerRequest::UpdateChannel { channel_id: 1, name: Some("renamed".into()), topic: None, nsfw: None, slow_mode_secs: None, retention_secs: None, category_id: None, position: None },
            ServerRequest::DeleteChannel { channel_id: 1 },
            ServerRequest::CreateCategory { name: "General".into(), position: Some(0) },
            ServerRequest::UpdateCategory { category_id: 1, name: Some("Renamed".into()), position: None },
            ServerRequest::DeleteCategory { category_id: 1 },
            ServerRequest::CreateRole { name: "Mod".into(), permissions: 0xFF, color: Some("#00FF00".into()), position: Some(2), hoist: None },
            ServerRequest::UpdateRole { role_id: 1, name: None, permissions: Some(0xFFFF), color: None, position: None, hoist: None },
            ServerRequest::DeleteRole { role_id: 1 },
            ServerRequest::AssignRole { member_key: kp.public_key(), role_id: 1 },
            ServerRequest::RemoveRole { member_key: kp.public_key(), role_id: 1 },
            ServerRequest::KickMember { member_key: kp.public_key() },
            ServerRequest::BanMember { member_key: kp.public_key(), reason: Some("spam".into()) },
            ServerRequest::UnbanMember { member_key: kp.public_key() },
            ServerRequest::TimeoutMember { member_key: kp.public_key(), until_ms: 9999, reason: Some("test".into()) },
            ServerRequest::RemoveTimeout { member_key: kp.public_key() },
            ServerRequest::ListAuditEvents { before_id: Some(100), limit: 50 },
            ServerRequest::ListBanned,
            ServerRequest::CreateInvite { max_uses: Some(10), expires_in_secs: Some(3600), target_channel: Some(1) },
            ServerRequest::GetServerInfo,
            ServerRequest::GetMembers,
            ServerRequest::SetChannelOverride { channel_id: 1, role_id: 2, allow: 0x03, deny: 0x04 },
            ServerRequest::SetCategoryOverride { category_id: 1, role_id: 2, allow: 0x03, deny: 0x04 },
            ServerRequest::CreateThread { message_id: 1, name: Some("thread".into()) },
            ServerRequest::AddReaction { message_id: 1, emoji: "👍".into(), file_id: None },
            ServerRequest::RemoveReaction { message_id: 1, emoji: "👍".into(), file_id: None },
            ServerRequest::RequestDeletion,
            ServerRequest::CancelDeletion,
            ServerRequest::GetDeletionStatus,
            ServerRequest::FetchUrl { url: "https://example.com/img.png".into(), channel_id: 1 },
            ServerRequest::OpenDm { target_key: kp.public_key() },
            ServerRequest::ListDms,
            ServerRequest::BlockUser { target_key: kp.public_key() },
            ServerRequest::UnblockUser { target_key: kp.public_key() },
            ServerRequest::JoinStream { channel_id: 7 },
            ServerRequest::LeaveStream,
            ServerRequest::EnableTrack { kind: TrackKind::Audio },
            ServerRequest::DisableTrack { kind: TrackKind::Video },
            ServerRequest::SetDeafen { deafened: true },
            ServerRequest::SetMute { muted: true },
            ServerRequest::OfferStreamKey {
                kind: TrackKind::Audio,
                wrapped_keys: vec![(kp.public_key(), vec![1, 2, 3, 4])],
            },
            ServerRequest::JoinChannelMedia { channel_id: 7 },
            ServerRequest::LeaveChannelMedia { channel_id: 7 },
            ServerRequest::GetMediaState { channel_id: 7 },
            ServerRequest::AddBotAlert { bot_public_key: kp.public_key(), metric: "price_usd".into(), comparator: "above".into(), threshold: 70000.0 },
            ServerRequest::RemoveBotAlert { alert_id: 1 },
            ServerRequest::ListBotAlerts { bot_public_key: kp.public_key() },
            ServerRequest::SubscribeBot { bot_public_key: kp.public_key() },
            ServerRequest::UnsubscribeBot { bot_public_key: kp.public_key() },
            ServerRequest::ListMySubscriptions,
        ];
        for req in requests {
            let frame = ClientFrame::Request { id: 1, body: req };
            let bytes = codec::encode(&frame).unwrap();
            let _decoded: ClientFrame = codec::decode(&bytes).unwrap();
        }

        let events = vec![
            ServerEvent::MediaJoined { channel_id: 1, public_key: kp.public_key(), display_name: "alice".into() },
            ServerEvent::MediaLeft   { channel_id: 1, public_key: kp.public_key() },
            ServerEvent::StreamJoined {
                channel_id: 1,
                public_key: kp.public_key(),
                display_name: "alice".into(),
                session_id: [9u8; 16],
                active_tracks: vec![TrackKind::Audio],
                muted: false,
                deafened: false,
            },
            ServerEvent::StreamLeft { channel_id: 1, session_id: [9u8; 16] },
            ServerEvent::TrackEnabled  { channel_id: 1, session_id: [9u8; 16], kind: TrackKind::Audio },
            ServerEvent::TrackDisabled { channel_id: 1, session_id: [9u8; 16], kind: TrackKind::Video },
            ServerEvent::TrackActivityChanged {
                channel_id: 1, session_id: [9u8; 16], kind: TrackKind::Audio, active: true,
            },
            ServerEvent::StreamCallIncoming {
                channel_id: 1, caller: kp.public_key(), caller_name: "alice".into(),
            },
            ServerEvent::StreamCallEnded { channel_id: 1 },
            ServerEvent::StreamKeyOffer {
                channel_id: 1,
                sender: kp.public_key(),
                session_id: [9u8; 16],
                kind: TrackKind::Audio,
                wrapped_key: vec![10, 11, 12],
            },
        ];
        for ev in events {
            let frame = ServerFrame::Event(ev);
            let bytes = codec::encode(&frame).unwrap();
            let _decoded: ServerFrame = codec::decode(&bytes).unwrap();
        }
    }

    #[test]
    fn test_roundtrip_relay_stream_role() {
        let p = RelayStreamRole::Primary;
        let back: RelayStreamRole = codec::decode(&codec::encode(&p).unwrap()).unwrap();
        assert!(matches!(back, RelayStreamRole::Primary));

        let s = RelayStreamRole::Session { token: vec![1u8, 2, 3] };
        let back: RelayStreamRole = codec::decode(&codec::encode(&s).unwrap()).unwrap();
        match back {
            RelayStreamRole::Session { token } => assert_eq!(token, vec![1u8, 2, 3]),
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_upload_request() {
        let req = UploadRequest {
            channel_id: 42,
            file_name: "photo.png".to_string(),
            file_size: 1024,
            hash: "abc123".to_string(),
            mime_type: "image/png".to_string(),
            width: Some(800),
            height: Some(600),
            duration_secs: None,
        };
        let bytes = codec::encode(&req).unwrap();
        let decoded: UploadRequest = codec::decode(&bytes).unwrap();
        assert_eq!(decoded.channel_id, 42);
        assert_eq!(decoded.file_name, "photo.png");
        assert_eq!(decoded.width, Some(800));
        assert!(decoded.duration_secs.is_none());
    }

    #[test]
    fn test_roundtrip_upload_response() {
        let variants = vec![
            UploadResponse::Ready,
            UploadResponse::Complete { file_id: 99 },
            UploadResponse::Error { reason: "too large".to_string() },
        ];
        for variant in variants {
            let bytes = codec::encode(&variant).unwrap();
            let _decoded: UploadResponse = codec::decode(&bytes).unwrap();
        }
        let complete = UploadResponse::Complete { file_id: 99 };
        let bytes = codec::encode(&complete).unwrap();
        let decoded: UploadResponse = codec::decode(&bytes).unwrap();
        match decoded {
            UploadResponse::Complete { file_id } => assert_eq!(file_id, 99),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_download_request() {
        let req = DownloadRequest { file_id: 7 };
        let bytes = codec::encode(&req).unwrap();
        let decoded: DownloadRequest = codec::decode(&bytes).unwrap();
        assert_eq!(decoded.file_id, 7);
    }

    #[test]
    fn test_roundtrip_reaction_group() {
        let rg = ReactionGroup {
            emoji: "👍".to_string(),
            count: 5,
            me: true,
            file_id: None,
        };
        let bytes = codec::encode(&rg).unwrap();
        let decoded: ReactionGroup = codec::decode(&bytes).unwrap();
        assert_eq!(decoded.emoji, "👍");
        assert_eq!(decoded.count, 5);
        assert!(decoded.me);
    }

    #[test]
    fn test_roundtrip_create_thread() {
        let frame = ClientFrame::Request {
            id: 99,
            body: ServerRequest::CreateThread {
                message_id: 42,
                name: Some("my thread".to_string()),
            },
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { id, body } => {
                assert_eq!(id, 99);
                match body {
                    ServerRequest::CreateThread { message_id, name } => {
                        assert_eq!(message_id, 42);
                        assert_eq!(name, Some("my thread".to_string()));
                    }
                    _ => panic!("wrong request variant"),
                }
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn presence_roundtrips() {
        let p = Presence { kind: PresenceKind::Music, details: "Song".into(), state: Some("Artist".into()) };
        let bytes = codec::encode(&p).unwrap();
        let back: Presence = codec::decode(&bytes).unwrap();
        assert_eq!(p, back);
        // None clears
        let req = ServerRequest::UpdatePresence { presence: None };
        let b = codec::encode(&req).unwrap();
        let _back: ServerRequest = codec::decode(&b).unwrap();
    }

    #[test]
    fn test_roundtrip_deletion_status() {
        let status = DeletionStatus {
            pending: true,
            requested_at: Some(1_000_000),
            expires_at: Some(1_000_000 + 72 * 3600),
        };
        let bytes = codec::encode(&status).unwrap();
        let decoded: DeletionStatus = codec::decode(&bytes).unwrap();
        assert_eq!(decoded, status);

        // Also test the non-pending variant.
        let status_none = DeletionStatus {
            pending: false,
            requested_at: None,
            expires_at: None,
        };
        let bytes2 = codec::encode(&status_none).unwrap();
        let decoded2: DeletionStatus = codec::decode(&bytes2).unwrap();
        assert_eq!(decoded2, status_none);
    }

    #[test]
    fn test_profile_protocol_roundtrip() {
        let kp = farder_crypto::identity::Keypair::generate();
        let req = ClientFrame::Request {
            id: 7,
            body: ServerRequest::UpdateProfile { profile: vec![1, 2, 3] },
        };
        let bytes = codec::encode(&req).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { id: 7, body: ServerRequest::UpdateProfile { profile } } => {
                assert_eq!(profile, vec![1, 2, 3]);
            }
            other => panic!("unexpected decode: {:?}", other),
        }

        let ev = ServerFrame::Event(ServerEvent::MemberProfileUpdated {
            public_key: kp.public_key(),
            profile_hash: Some("ab".repeat(32)),
        });
        let bytes = codec::encode(&ev).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        let expected_hash = "ab".repeat(32);
        match decoded {
            ServerFrame::Event(ServerEvent::MemberProfileUpdated { public_key, profile_hash }) => {
                assert_eq!(public_key, kp.public_key());
                assert_eq!(profile_hash.as_deref(), Some(expected_hash.as_str()));
            }
            other => panic!("unexpected decode: {:?}", other),
        }

        let resp = ServerResponse::MemberProfile { member_key: kp.public_key(), profile: None };
        let bytes = codec::encode(&resp).unwrap();
        let decoded: ServerResponse = codec::decode(&bytes).unwrap();
        match decoded {
            ServerResponse::MemberProfile { member_key, profile } => {
                assert_eq!(member_key, kp.public_key());
                assert!(profile.is_none());
            }
            other => panic!("unexpected decode: {:?}", other),
        }
    }

    #[test]
    fn test_roundtrip_message_info_with_attachments() {
        let kp = Keypair::generate();
        let msg = MessageInfo {
            id: 10,
            channel_id: 1,
            author: kp.public_key(),
            content: "check out this file".to_string(),
            timestamp: 5000,
            edited_at: None,
            reply_to: None,
            pinned: false,
            attachments: vec![
                AttachmentInfo {
                    id: 1,
                    file_id: 42,
                    name: "document.pdf".to_string(),
                    size: 2048,
                    mime_type: "application/pdf".to_string(),
                    width: None,
                    height: None,
                    duration_secs: None,
                    content_hash: String::new(),
                    redacted_by_moderator: None,
                },
            ],
            reactions: vec![],
            thread_id: None,
            thread_message_count: None,
            author_name_override: None,
            author_badge: None,
        };
        let bytes = codec::encode(&msg).unwrap();
        let decoded: MessageInfo = codec::decode(&bytes).unwrap();
        assert_eq!(decoded.id, 10);
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].file_id, 42);
        assert_eq!(decoded.attachments[0].name, "document.pdf");
    }
}
