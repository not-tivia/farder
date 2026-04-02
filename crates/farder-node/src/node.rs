use anyhow::Result;
use farder_crypto::encryption;
use farder_crypto::identity::{Keypair, PublicKey};
use farder_crypto::key_exchange::SessionKeypair;
use farder_protocol::messages::Message;
use crate::message_store::LocalMessageStore;
use crate::peer_manager::PeerManager;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct PersonalNode {
    pub keypair: Keypair,
    pub peer_manager: PeerManager,
    pub message_store: LocalMessageStore,
    session_keypair: SessionKeypair,
}

impl PersonalNode {
    pub fn new(keypair: Keypair, db_path: &str) -> Result<Self> {
        Ok(Self {
            keypair,
            peer_manager: PeerManager::new(),
            message_store: LocalMessageStore::open(db_path)?,
            session_keypair: SessionKeypair::generate(),
        })
    }

    pub fn new_in_memory(keypair: Keypair) -> Result<Self> {
        Ok(Self {
            keypair,
            peer_manager: PeerManager::new(),
            message_store: LocalMessageStore::open_in_memory()?,
            session_keypair: SessionKeypair::generate(),
        })
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public_key()
    }

    pub fn session_public_key_bytes(&self) -> [u8; 32] {
        self.session_keypair.public_key_bytes()
    }

    pub fn complete_key_exchange(&mut self, peer: &PublicKey, peer_session_public_key: &[u8; 32]) {
        let peer_pk = x25519_dalek::PublicKey::from(*peer_session_public_key);
        let shared_secret = self.session_keypair.derive_shared_secret(&peer_pk);
        self.peer_manager.add_peer(peer.clone());
        self.peer_manager.set_shared_secret(peer, shared_secret);
    }

    pub fn prepare_dm(&self, recipient: &PublicKey, plaintext: &[u8]) -> Result<Message> {
        let peer = self.peer_manager.get_peer(recipient)
            .ok_or_else(|| anyhow::anyhow!("no session with peer"))?;
        let secret = peer.shared_secret
            .ok_or_else(|| anyhow::anyhow!("key exchange not completed"))?;
        let ciphertext = encryption::encrypt(&secret, plaintext)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.message_store.save_message(recipient, true, &ciphertext, timestamp)?;
        Ok(Message::EncryptedDm {
            sender: self.keypair.public_key(),
            ciphertext,
            timestamp,
        })
    }

    pub fn receive_dm(&self, sender: &PublicKey, ciphertext: &[u8], timestamp: u64) -> Result<Vec<u8>> {
        let peer = self.peer_manager.get_peer(sender)
            .ok_or_else(|| anyhow::anyhow!("no session with peer"))?;
        let secret = peer.shared_secret
            .ok_or_else(|| anyhow::anyhow!("key exchange not completed"))?;
        let plaintext = encryption::decrypt(&secret, ciphertext)?;
        self.message_store.save_message(sender, false, ciphertext, timestamp)?;
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_dm_flow() {
        let alice_keypair = Keypair::generate();
        let bob_keypair = Keypair::generate();

        let mut alice = PersonalNode::new_in_memory(alice_keypair).unwrap();
        let mut bob = PersonalNode::new_in_memory(bob_keypair).unwrap();

        let alice_session_pk = alice.session_public_key_bytes();
        let bob_session_pk = bob.session_public_key_bytes();

        alice.complete_key_exchange(&bob.public_key(), &bob_session_pk);
        bob.complete_key_exchange(&alice.public_key(), &alice_session_pk);

        let msg = alice.prepare_dm(&bob.public_key(), b"Hello Bob!").unwrap();

        let (ciphertext, timestamp) = match msg {
            Message::EncryptedDm { ciphertext, timestamp, .. } => (ciphertext, timestamp),
            _ => panic!("expected EncryptedDm"),
        };

        let decrypted = bob.receive_dm(&alice.public_key(), &ciphertext, timestamp).unwrap();
        assert_eq!(decrypted, b"Hello Bob!");
    }

    #[test]
    fn test_dm_without_key_exchange_fails() {
        let alice_keypair = Keypair::generate();
        let bob_keypair = Keypair::generate();

        let alice = PersonalNode::new_in_memory(alice_keypair).unwrap();
        let bob_pub = bob_keypair.public_key();

        let result = alice.prepare_dm(&bob_pub, b"Hello Bob!");
        assert!(result.is_err());
    }

    #[test]
    fn test_messages_stored_locally() {
        let alice_keypair = Keypair::generate();
        let bob_keypair = Keypair::generate();

        let mut alice = PersonalNode::new_in_memory(alice_keypair).unwrap();
        let mut bob = PersonalNode::new_in_memory(bob_keypair).unwrap();

        let alice_session_pk = alice.session_public_key_bytes();
        let bob_session_pk = bob.session_public_key_bytes();

        alice.complete_key_exchange(&bob.public_key(), &bob_session_pk);
        bob.complete_key_exchange(&alice.public_key(), &alice_session_pk);

        alice.prepare_dm(&bob.public_key(), b"msg1").unwrap();
        alice.prepare_dm(&bob.public_key(), b"msg2").unwrap();
        alice.prepare_dm(&bob.public_key(), b"msg3").unwrap();

        let stored = alice.message_store.get_messages(&bob.public_key(), 10, 0).unwrap();
        assert_eq!(stored.len(), 3);
        assert!(stored.iter().all(|m| m.is_outgoing));
    }
}
