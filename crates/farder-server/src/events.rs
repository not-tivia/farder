use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;

#[derive(Debug)]
pub enum EventTarget {
    All,
    Subscribers(u64),
    Members(Vec<PublicKey>),
    PermissionHolders(u64),
    /// Internal signal: mutate voice state. The `event` field on the
    /// containing BroadcastEvent is ignored for these — they don't emit to clients.
    VoiceStartTransmit { pk: [u8; 32], channel_id: u64 },
    VoiceStopTransmit { pk: [u8; 32] },
    VoiceSetMute { pk: [u8; 32], muted: bool },
    VoiceSetDeafen { pk: [u8; 32], deafened: bool },
    // Media-stream targets (new). Coexist with the Voice* variants during
    // the MST migration; Voice* are removed in MST-11 once handlers no
    // longer reference them.
    MediaStreamJoin { session_id: [u8; 16], channel_id: u64, public_key: [u8; 32] },
    MediaStreamLeave { session_id: [u8; 16] },
    MediaTrackEnabled { session_id: [u8; 16], channel_id: u64, kind: farder_protocol::server::TrackKind },
    MediaTrackDisabled { session_id: [u8; 16], channel_id: u64, kind: farder_protocol::server::TrackKind },
    MediaSetDeafen { session_id: [u8; 16], deafened: bool },
}

#[derive(Debug)]
pub struct BroadcastEvent {
    pub target: EventTarget,
    pub event: ServerEvent,
}
