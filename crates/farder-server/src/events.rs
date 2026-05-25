use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;

#[derive(Debug)]
pub enum EventTarget {
    All,
    Subscribers(u64),
    Members(Vec<PublicKey>),
    PermissionHolders(u64),
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
