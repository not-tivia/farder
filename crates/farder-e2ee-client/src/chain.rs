//! Event-chain state and event construction.
//!
//! The vertical builds and signs every event exactly the way the Tauri client
//! does in `client/src-tauri/src/commands.rs` (`event_build_next`): an
//! [`EventCore`] with `server_id` = the log server id (genesis hash), `author`
//! = the identity public key, `device` = `device_id(device_pubkey)`, and
//! `(seq, prev, lamport)` taken from per-(server, device) chain state, then
//! [`Event::sign`] with the device subkey.
//!
//! This crate owns **no storage** for that state: [`ChainState`] is a plain
//! struct the caller supplies and persists. The Tauri layer owns the real
//! `device_state.json` file (sub-project 4b); the harness owns its own copy
//! (sub-project 4a). This module never reads or writes `device_state.json`.

use farder_crypto::event_log::{device_id, Event, EventCore, EventHash, EventPayload};
use farder_crypto::identity::Keypair;

/// The "who" of the vertical: the acting device subkey, its owning identity,
/// and the log server it acts on. Bundles the three inputs every event build
/// needs, so the lifecycle functions stay under the 7-argument clippy bound
/// and the harness constructs one per identity.
pub struct Actor<'a> {
    /// The device subkey: signs events and doubles as the MLS leaf key.
    pub device: &'a Keypair,
    /// The owning identity: `EventCore.author` and the MLS credential identity.
    pub identity: &'a Keypair,
    /// The log server id (genesis hash): `EventCore.server_id`.
    pub log_server_id: &'a str,
}

/// Per-(server, device) event-chain state — the same three fields the Tauri
/// client persists in `device_state.json` (`next_seq`, `last_event_hash`,
/// `lamport`), kept as a plain struct so any caller can round-trip it through
/// any store.
///
/// `next_seq` is the `core.seq` the next event will carry; `last_event_hash`
/// is `prev` for the next event (the chain head, `None` for the first event);
/// `lamport` is the highest lamport this device has observed, so the next
/// event carries `lamport + 1`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainState {
    pub next_seq: u64,
    pub last_event_hash: Option<EventHash>,
    pub lamport: u64,
}

impl ChainState {
    /// Advance this chain state past `event` after the server accepted it.
    ///
    /// Mirrors the Tauri client's post-accept update
    /// (`ds.next_seq = event.core.seq + 1; ds.last_event_hash = Some(event.hash());
    /// ds.lamport = event.core.lamport`). Call only after a successful submit.
    pub fn advance(&mut self, event: &Event) {
        self.next_seq = event.core.seq + 1;
        self.last_event_hash = Some(event.hash());
        self.lamport = event.core.lamport;
    }
}

/// Build and sign the next event in this device's chain. Pure: fully
/// determined by the inputs, no I/O.
pub fn build_next_event(
    device: &Keypair,
    identity: &Keypair,
    server_id: &str,
    chain: &ChainState,
    timestamp: u64,
    payload: EventPayload,
) -> Event {
    let core = EventCore {
        server_id: server_id.to_string(),
        author: identity.public_key(),
        device: device_id(&device.public_key()),
        seq: chain.next_seq,
        prev: chain.last_event_hash.clone(),
        lamport: chain.lamport + 1,
        timestamp,
        payload,
    };
    Event::sign(core, device)
}

/// The device wall-clock now, in unix seconds — the untrusted `core.timestamp`
/// claim every event carries (ingest bounds it to 300 s ahead of server time).
pub fn event_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::EventPayload;

    fn msg_payload(n: u64) -> EventPayload {
        EventPayload::MessagePosted {
            channel_id: 1,
            content: format!("m{n}"),
            reply_to: None,
            attachments: vec![],
        }
    }

    #[test]
    fn build_next_event_chains_signs_and_verifies() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let mut chain = ChainState::default();

        let e0 = build_next_event(
            &device,
            &identity,
            "server-hex",
            &chain,
            100,
            msg_payload(0),
        );
        assert_eq!(e0.core.seq, 0);
        assert_eq!(e0.core.prev, None);
        assert_eq!(e0.core.lamport, 1);
        assert_eq!(e0.core.server_id, "server-hex");
        assert_eq!(e0.core.author, identity.public_key());
        assert_eq!(e0.core.device, device_id(&device.public_key()));
        assert!(e0.verify(&device.public_key()).is_ok());

        chain.advance(&e0);
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(e0.hash().as_str()));
        assert_eq!(chain.lamport, 1);

        let e1 = build_next_event(
            &device,
            &identity,
            "server-hex",
            &chain,
            101,
            msg_payload(1),
        );
        assert_eq!(e1.core.seq, 1);
        assert_eq!(e1.core.prev.as_deref(), Some(e0.hash().as_str()));
        assert_eq!(e1.core.lamport, 2);
        assert!(e1.verify(&device.public_key()).is_ok());
        assert_ne!(e1.hash(), e0.hash());
    }

    #[test]
    fn a_second_device_runs_an_independent_chain_under_the_same_identity() {
        let identity = Keypair::generate();
        let dev_a = Keypair::generate();
        let dev_b = Keypair::generate();
        let chain = ChainState::default();

        let a = build_next_event(&dev_a, &identity, "srv", &chain, 1, msg_payload(0));
        let b = build_next_event(&dev_b, &identity, "srv", &chain, 1, msg_payload(0));
        assert_eq!(a.core.author, b.core.author);
        assert_ne!(a.core.device, b.core.device);
        assert_ne!(a.hash(), b.hash());
    }
}
