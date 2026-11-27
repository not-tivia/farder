//! Hosting terms: the owner's signed statement "from term N on, machine K is the host,
//! continuing after event H". See `docs/specs/2026-11-27-promotion-design.md`.
//!
//! The signed bytes are the POSITIONAL `rmp_serde::to_vec` of the record without its signature
//! (like events and device certs). They must never go through the wire codec
//! (`farder_protocol::codec::encode` writes named maps): see "Compatibility" in
//! `docs/modules/protocol.md`.

use crate::identity::{Keypair, PublicKey};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// An owner-signed hosting term. Term 0 is the original host and is implicit (never signed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostTerm {
    /// The server's genesis hash (`Genesis::server_id`).
    pub server_id: String,
    /// Each promotion is the previous term + 1.
    pub term: u64,
    /// The new host's server identity: the 32-byte id in its `--data-dir` (`server_id` file).
    pub host_key: [u8; 32],
    /// Where the new host can be reached (informational; not checked by the server).
    pub addresses: Vec<String>,
    /// Hash of the newest event the new host holds; it continues from here.
    pub after_event: String,
    /// Unix seconds, the owner's clock (informational).
    pub issued_at: u64,
    /// The owner identity's Ed25519 signature over [`HostTerm::signed_bytes`].
    pub signature: Vec<u8>,
}

/// Everything but the signature, in the same field order as [`HostTerm`] (the order IS the format).
#[derive(Serialize)]
struct HostTermBody<'a> {
    server_id: &'a str,
    term: u64,
    host_key: &'a [u8; 32],
    addresses: &'a [String],
    after_event: &'a str,
    issued_at: u64,
}

impl HostTerm {
    /// Build and sign a term with the owner's identity key.
    pub fn sign(
        owner: &Keypair,
        server_id: String,
        term: u64,
        host_key: [u8; 32],
        addresses: Vec<String>,
        after_event: String,
        issued_at: u64,
    ) -> Self {
        let mut t = Self { server_id, term, host_key, addresses, after_event, issued_at, signature: Vec::new() };
        t.signature = owner.sign(&t.signed_bytes());
        t
    }

    /// The bytes the owner signs: positional msgpack of every field except the signature.
    pub fn signed_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(&HostTermBody {
            server_id: &self.server_id,
            term: self.term,
            host_key: &self.host_key,
            addresses: &self.addresses,
            after_event: &self.after_event,
            issued_at: self.issued_at,
        })
        .expect("host term serialization cannot fail")
    }

    /// Ok iff `owner` signed exactly these fields. Term 0 is never signed, so it never verifies.
    pub fn verify(&self, owner: &PublicKey) -> Result<()> {
        if self.term == 0 {
            bail!("term 0 is the original host and is never signed");
        }
        owner.verify(&self.signed_bytes(), &self.signature).context("host term is not signed by the owner")
    }

    /// Storage form (positional msgpack of the whole record, signature included).
    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("host term serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("failed to decode host term")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(owner: &Keypair) -> HostTerm {
        HostTerm::sign(owner, "srv".into(), 1, [7; 32], vec!["10.0.0.2:4435".into()], "abc".into(), 1000)
    }

    #[test]
    fn a_valid_term_verifies() {
        let owner = Keypair::generate();
        sample(&owner).verify(&owner.public_key()).unwrap();
    }

    #[test]
    fn any_changed_field_fails() {
        let owner = Keypair::generate();
        let good = sample(&owner);
        let mut cases = Vec::new();
        let mut t = good.clone(); t.server_id = "other".into(); cases.push(("server_id", t));
        let mut t = good.clone(); t.term = 2; cases.push(("term", t));
        let mut t = good.clone(); t.host_key[0] ^= 1; cases.push(("host_key", t));
        let mut t = good.clone(); t.addresses.push("x".into()); cases.push(("addresses", t));
        let mut t = good.clone(); t.after_event = "abd".into(); cases.push(("after_event", t));
        let mut t = good.clone(); t.issued_at += 1; cases.push(("issued_at", t));
        let mut t = good.clone(); t.signature[0] ^= 1; cases.push(("signature", t));
        for (name, t) in cases {
            assert!(t.verify(&owner.public_key()).is_err(), "changing {name} must fail verification");
        }
    }

    #[test]
    fn another_key_fails() {
        let owner = Keypair::generate();
        assert!(sample(&owner).verify(&Keypair::generate().public_key()).is_err());
    }

    #[test]
    fn term_zero_never_verifies() {
        let owner = Keypair::generate();
        let t = HostTerm::sign(&owner, "srv".into(), 0, [7; 32], vec![], "abc".into(), 1);
        assert!(t.verify(&owner.public_key()).is_err());
    }

    #[test]
    fn signed_bytes_are_positional_and_storage_roundtrips() {
        let owner = Keypair::generate();
        let t = sample(&owner);
        // Positional: a msgpack ARRAY of the six fields (0x96), not a map.
        assert_eq!(t.signed_bytes()[0], 0x96);
        let back = HostTerm::from_bytes(&t.to_bytes()).unwrap();
        assert_eq!(back, t);
        back.verify(&owner.public_key()).unwrap();
    }
}
