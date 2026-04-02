use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};
use rand::rngs::OsRng;

pub struct SessionKeypair {
    secret: StaticSecret,
    public: PublicKey,
}

impl SessionKeypair {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    pub fn derive_shared_secret(&self, peer_public: &PublicKey) -> [u8; 32] {
        let shared: SharedSecret = self.secret.diffie_hellman(peer_public);
        *shared.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_secret_agreement() {
        let alice = SessionKeypair::generate();
        let bob = SessionKeypair::generate();

        let alice_shared = alice.derive_shared_secret(bob.public_key());
        let bob_shared = bob.derive_shared_secret(alice.public_key());

        assert_eq!(alice_shared, bob_shared);
    }

    #[test]
    fn test_different_keypairs_different_secrets() {
        let alice = SessionKeypair::generate();
        let bob = SessionKeypair::generate();
        let charlie = SessionKeypair::generate();

        let alice_bob = alice.derive_shared_secret(bob.public_key());
        let alice_charlie = alice.derive_shared_secret(charlie.public_key());

        assert_ne!(alice_bob, alice_charlie);
    }

    #[test]
    fn test_session_public_key_serialization() {
        let keypair = SessionKeypair::generate();
        let bytes = keypair.public_key_bytes();
        assert_eq!(bytes.len(), 32);
    }
}
