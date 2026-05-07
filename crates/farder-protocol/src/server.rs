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
    CreateRole { name: String, permissions: u64, color: Option<String>, position: Option<u32> },
    UpdateRole { role_id: u64, name: Option<String>, permissions: Option<u64>, color: Option<String>, position: Option<u32> },
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
    GetServerInfo,
    GetMembers,
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
    JoinVoice { channel_id: u64 },
    LeaveVoice { channel_id: u64 },
    GetVoiceState { channel_id: u64 },
    StartVoice { channel_id: u64 },
    StopVoice,
    SetVoiceMute { muted: bool },
    SetVoiceDeafen { deafened: bool },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerFrame {
    Challenge { nonce: [u8; 32] },
    Authenticated { session_token: Vec<u8> },
    AuthError { reason: String },
    Response { request_id: u32, body: ServerResponse },
    Event(ServerEvent),
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
    },
    Members { members: Vec<MemberInfo> },
    BannedMembers {
        entries: Vec<BannedMember>,
    },
    AuditEventsList { events: Vec<AuditEvent> },
    InviteCreated { code: String },
    DeletionStatusResp { status: DeletionStatus },
    UrlFetched { file_id: u64 },
    DmOpened { channel: ChannelInfo, participant: MemberInfo },
    DmList { dms: Vec<DmEntry> },
    VoiceStateResp { participants: Vec<VoiceMember> },
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
    VoiceJoined { channel_id: u64, public_key: PublicKey, display_name: String },
    VoiceLeft { channel_id: u64, public_key: PublicKey },
    VoiceCallIncoming {
        channel_id: u64,
        caller: PublicKey,
        caller_name: String,
    },
    VoiceCallEnded {
        channel_id: u64,
    },
    VoiceSpeakingChanged {
        channel_id: u64,
        public_key: PublicKey,
        speaking: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;
    use farder_crypto::identity::Keypair;

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
            ServerRequest::CreateRole { name: "Mod".into(), permissions: 0xFF, color: Some("#00FF00".into()), position: Some(2) },
            ServerRequest::UpdateRole { role_id: 1, name: None, permissions: Some(0xFFFF), color: None, position: None },
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
            ServerRequest::JoinVoice { channel_id: 1 },
            ServerRequest::LeaveVoice { channel_id: 1 },
            ServerRequest::GetVoiceState { channel_id: 1 },
            ServerRequest::StartVoice { channel_id: 1 },
            ServerRequest::StopVoice,
            ServerRequest::SetVoiceMute { muted: true },
            ServerRequest::SetVoiceDeafen { deafened: false },
        ];
        for req in requests {
            let frame = ClientFrame::Request { id: 1, body: req };
            let bytes = codec::encode(&frame).unwrap();
            let _decoded: ClientFrame = codec::decode(&bytes).unwrap();
        }

        let events = vec![
            ServerEvent::VoiceCallIncoming { channel_id: 1, caller: kp.public_key(), caller_name: "alice".into() },
            ServerEvent::VoiceCallEnded { channel_id: 1 },
            ServerEvent::VoiceSpeakingChanged { channel_id: 1, public_key: kp.public_key(), speaking: true },
        ];
        for ev in events {
            let frame = ServerFrame::Event(ev);
            let bytes = codec::encode(&frame).unwrap();
            let _decoded: ServerFrame = codec::decode(&bytes).unwrap();
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
                },
            ],
            reactions: vec![],
            thread_id: None,
            thread_message_count: None,
        };
        let bytes = codec::encode(&msg).unwrap();
        let decoded: MessageInfo = codec::decode(&bytes).unwrap();
        assert_eq!(decoded.id, 10);
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].file_id, 42);
        assert_eq!(decoded.attachments[0].name, "document.pdf");
    }
}
