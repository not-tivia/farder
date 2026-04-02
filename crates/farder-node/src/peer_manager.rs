use farder_crypto::identity::PublicKey;
use std::collections::HashMap;

pub struct PeerManager {
    peers: HashMap<[u8; 32], PeerState>,
}

pub struct PeerState {
    pub public_key: PublicKey,
    pub display_name: Option<String>,
    pub shared_secret: Option<[u8; 32]>,
    pub relay_address: Option<String>,
    pub online: bool,
}

impl PeerManager {
    pub fn new() -> Self {
        Self { peers: HashMap::new() }
    }

    pub fn add_peer(&mut self, public_key: PublicKey) {
        let key = *public_key.as_bytes();
        self.peers.entry(key).or_insert_with(|| PeerState {
            public_key,
            display_name: None,
            shared_secret: None,
            relay_address: None,
            online: false,
        });
    }

    pub fn get_peer(&self, public_key: &PublicKey) -> Option<&PeerState> {
        self.peers.get(public_key.as_bytes())
    }

    pub fn get_peer_mut(&mut self, public_key: &PublicKey) -> Option<&mut PeerState> {
        self.peers.get_mut(public_key.as_bytes())
    }

    pub fn set_shared_secret(&mut self, public_key: &PublicKey, secret: [u8; 32]) {
        if let Some(peer) = self.get_peer_mut(public_key) {
            peer.shared_secret = Some(secret);
        }
    }

    pub fn list_peers(&self) -> Vec<&PeerState> {
        self.peers.values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    fn make_key() -> PublicKey {
        Keypair::generate().public_key()
    }

    #[test]
    fn test_add_and_get_peer() {
        let mut manager = PeerManager::new();
        let key = make_key();
        manager.add_peer(key.clone());
        let peer = manager.get_peer(&key);
        assert!(peer.is_some());
        assert_eq!(peer.unwrap().public_key.as_bytes(), key.as_bytes());
    }

    #[test]
    fn test_set_shared_secret() {
        let mut manager = PeerManager::new();
        let key = make_key();
        manager.add_peer(key.clone());
        let secret = [42u8; 32];
        manager.set_shared_secret(&key, secret);
        let peer = manager.get_peer(&key).unwrap();
        assert_eq!(peer.shared_secret, Some(secret));
    }

    #[test]
    fn test_unknown_peer_returns_none() {
        let manager = PeerManager::new();
        let unknown_key = make_key();
        assert!(manager.get_peer(&unknown_key).is_none());
    }
}
