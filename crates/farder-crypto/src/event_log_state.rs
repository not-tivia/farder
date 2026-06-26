//! The authorization state machine: folds a server's signed event log into the
//! current membership / bans / capabilities / devices / invites, validating every
//! event against the per-payload signing rules. Pure (no I/O), so it replays
//! deterministically and composes from any checkpoint.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};

use crate::event_log::{device_id, DeviceId, Event, EventHash, EventPayload, Genesis, ServerId};
use crate::identity::PublicKey;

/// A device authorized within this server's log (identity ↔ signing subkey).
#[derive(Clone, Debug)]
struct DeviceRecord {
    identity: PublicKey,
    device_pubkey: PublicKey,
}

/// An open invite, keyed in `LogState.invites` by its `InviteCreated` event hash.
#[derive(Clone, Debug)]
struct InviteRecord {
    max_uses: u32,
    expires_at: u64,
    use_count: u32,
}

/// Head of one `(author, device)` chain.
#[derive(Clone, Debug)]
struct ChainHead {
    seq: u64,
    hash: EventHash,
}

/// Authorization state derived by folding a server's event log. Construct only
/// via `from_genesis`, then advance with `apply`.
#[derive(Clone, Debug)]
pub struct LogState {
    server_id: ServerId,
    owner: PublicKey,
    members: HashSet<PublicKey>,
    banned: HashSet<PublicKey>,
    capabilities: HashMap<PublicKey, HashSet<String>>,
    devices: HashMap<DeviceId, DeviceRecord>,
    invites: HashMap<EventHash, InviteRecord>,
    chains: HashMap<(PublicKey, DeviceId), ChainHead>,
}

impl LogState {
    /// Initial state: the genesis owner is the sole member and root of authority.
    pub fn from_genesis(g: &Genesis) -> Self {
        let mut members = HashSet::new();
        members.insert(g.owner.clone());
        Self {
            server_id: g.server_id(),
            owner: g.owner.clone(),
            members,
            banned: HashSet::new(),
            capabilities: HashMap::new(),
            devices: HashMap::new(),
            invites: HashMap::new(),
            chains: HashMap::new(),
        }
    }

    pub fn server_id(&self) -> &ServerId {
        &self.server_id
    }
    pub fn owner(&self) -> &PublicKey {
        &self.owner
    }
    pub fn is_owner(&self, pk: &PublicKey) -> bool {
        pk == &self.owner
    }
    pub fn is_member(&self, pk: &PublicKey) -> bool {
        self.members.contains(pk)
    }
    pub fn is_banned(&self, pk: &PublicKey) -> bool {
        self.banned.contains(pk)
    }
    /// The owner holds every capability; everyone else holds only what was granted.
    pub fn has_capability(&self, pk: &PublicKey, cap: &str) -> bool {
        self.is_owner(pk) || self.capabilities.get(pk).map_or(false, |c| c.contains(cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Keypair;

    fn genesis(owner: &Keypair) -> Genesis {
        Genesis {
            version: 1,
            name: "t".to_string(),
            owner: owner.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        }
    }

    #[test]
    fn from_genesis_seeds_owner_as_member_with_all_authority() {
        let owner = Keypair::generate();
        let st = LogState::from_genesis(&genesis(&owner));
        let o = owner.public_key();
        assert_eq!(st.owner(), &o);
        assert!(st.is_owner(&o));
        assert!(st.is_member(&o));
        assert!(!st.is_banned(&o));
        // Owner implicitly holds any capability.
        assert!(st.has_capability(&o, "invite"));
        assert!(st.has_capability(&o, "ban"));
        assert!(st.has_capability(&o, "anything"));
        // A stranger holds nothing and is not a member.
        let stranger = Keypair::generate().public_key();
        assert!(!st.is_member(&stranger));
        assert!(!st.has_capability(&stranger, "invite"));
        assert_eq!(st.server_id(), &genesis(&owner).server_id());
    }
}
