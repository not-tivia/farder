use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};
use rand::rngs::OsRng;

/// Convert an Ed25519 signing key (32 raw secret bytes) to an X25519 StaticSecret.
///
/// This uses the standard SHA-512 + clamping derivation, which is the same
/// method used by libsodium's `crypto_sign_ed25519_sk_to_curve25519`.
pub fn ed25519_sk_to_x25519(ed_sk: &[u8; 32]) -> StaticSecret {
    use sha2::{Sha512, Digest};
    let h = Sha512::digest(ed_sk);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&h[..32]);
    // Clamp — as per RFC 7748 / X25519 spec
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    StaticSecret::from(scalar)
}

/// Convert an Ed25519 verifying key (compressed Edwards Y, 32 bytes) to an
/// X25519 PublicKey via the birational map from the Edwards curve to the
/// Montgomery curve (Curve25519).
///
/// This is the standard conversion used by libsodium and Signal.
pub fn ed25519_pk_to_x25519(ed_pk: &[u8; 32]) -> Result<PublicKey, &'static str> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    let compressed = CompressedEdwardsY::from_slice(ed_pk)
        .map_err(|_| "invalid ed25519 public key bytes")?;
    let point = compressed.decompress()
        .ok_or("failed to decompress ed25519 public key")?;
    let montgomery = point.to_montgomery();
    Ok(PublicKey::from(montgomery.to_bytes()))
}

/// Derive a symmetric shared secret for DM encryption between two users whose
/// identity keys are Ed25519 keypairs.
///
/// Both sides derive the same secret:
///   - A calls `derive_dm_shared_secret(&a_signing_key, &b_verifying_key)`
///   - B calls `derive_dm_shared_secret(&b_signing_key, &a_verifying_key)`
///
/// This works because X25519 ECDH is symmetric: `a_x * B_x == b_x * A_x`.
pub fn derive_dm_shared_secret(
    our_ed_sk: &[u8; 32],
    their_ed_pk: &[u8; 32],
) -> Result<[u8; 32], &'static str> {
    let x_secret = ed25519_sk_to_x25519(our_ed_sk);
    let x_public = ed25519_pk_to_x25519(their_ed_pk)?;
    let shared = x_secret.diffie_hellman(&x_public);
    Ok(*shared.as_bytes())
}

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

    #[test]
    fn test_derive_dm_shared_secret_symmetric() {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let a_sk = SigningKey::generate(&mut OsRng);
        let b_sk = SigningKey::generate(&mut OsRng);

        let a_pk = a_sk.verifying_key().to_bytes();
        let b_pk = b_sk.verifying_key().to_bytes();

        let secret_from_a = derive_dm_shared_secret(a_sk.as_bytes(), &b_pk).unwrap();
        let secret_from_b = derive_dm_shared_secret(b_sk.as_bytes(), &a_pk).unwrap();

        assert_eq!(secret_from_a, secret_from_b, "both parties must derive the same shared secret");
    }

    #[test]
    fn test_derive_dm_shared_secret_different_peers() {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let a_sk = SigningKey::generate(&mut OsRng);
        let b_sk = SigningKey::generate(&mut OsRng);
        let c_sk = SigningKey::generate(&mut OsRng);

        let b_pk = b_sk.verifying_key().to_bytes();
        let c_pk = c_sk.verifying_key().to_bytes();

        let a_b = derive_dm_shared_secret(a_sk.as_bytes(), &b_pk).unwrap();
        let a_c = derive_dm_shared_secret(a_sk.as_bytes(), &c_pk).unwrap();

        assert_ne!(a_b, a_c, "different peers must produce different secrets");
    }
}
