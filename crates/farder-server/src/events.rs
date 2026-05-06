use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;

#[derive(Debug)]
pub enum EventTarget {
    All,
    Subscribers(u64),
    Members(Vec<PublicKey>),
    PermissionHolders(u64),
}

#[derive(Debug)]
pub struct BroadcastEvent {
    pub target: EventTarget,
    pub event: ServerEvent,
}
