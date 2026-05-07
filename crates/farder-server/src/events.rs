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
}

#[derive(Debug)]
pub struct BroadcastEvent {
    pub target: EventTarget,
    pub event: ServerEvent,
}
