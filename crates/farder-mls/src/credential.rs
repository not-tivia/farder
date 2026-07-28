//! Credential binding between MLS leaves and farder-crypto device subkeys.
//!
//! The MLS leaf signs with the *device subkey* (the same Ed25519 `Keypair`
//! that signs log events) via [`DeviceSigner`]. The leaf's basic-credential
//! identity is the normative length-prefixed, domain-separated encoding
//! (spec M1):
//!
//! ```text
//! "farder-mls-cred-v1" || u8(len(identity_pubkey)) || identity_pubkey
//!                      || u32_be(len(device_id_bytes)) || device_id_bytes
//! ```
//!
//! Bare concatenation is forbidden (`DeviceId` is a hex `String`, `PublicKey`
//! is raw bytes — unprefixed concatenation would be ambiguous). Decoding is
//! strict: bad prefix, short reads, and trailing bytes are all errors.
//!
//! [`verify_leaf_binding`] checks the *cryptographic* binding
//! credential ↔ leaf signature key ↔ identity-signed [`DeviceCert`]. Fold
//! status (membership, revocation, expiry) is sub-2's job — the caller must
//! already trust the cert it passes in.

use anyhow::{bail, Context, Result};
use farder_crypto::event_log::{DeviceCert, DeviceId};
use farder_crypto::identity::{Keypair, PublicKey};
use openmls::prelude::{
    BasicCredential, Credential, CredentialWithKey, KeyPackage, KeyPackageBundle,
};
use openmls_traits::signatures::{Signer, SignerError};
use openmls_traits::types::SignatureScheme;
use openmls_traits::OpenMlsProvider;

use crate::CIPHERSUITE;

/// Domain-separation prefix for credential identity bytes (spec M1).
const CRED_PREFIX: &[u8] = b"farder-mls-cred-v1";

/// Encode `(identity pubkey, device id)` as the normative length-prefixed
/// credential identity bytes.
pub fn encode_credential_identity(identity: &PublicKey, device: &DeviceId) -> Vec<u8> {
    let pubkey = identity.as_bytes();
    let device_bytes = device.as_bytes();
    let mut out = Vec::with_capacity(CRED_PREFIX.len() + 1 + pubkey.len() + 4 + device_bytes.len());
    out.extend_from_slice(CRED_PREFIX);
    out.push(pubkey.len() as u8);
    out.extend_from_slice(pubkey);
    out.extend_from_slice(&(device_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(device_bytes);
    out
}

/// Strictly decode credential identity bytes produced by
/// [`encode_credential_identity`]. Rejects a wrong prefix, truncated input,
/// trailing bytes, and length fields that do not match exactly.
pub fn decode_credential_identity(bytes: &[u8]) -> Result<(PublicKey, DeviceId)> {
    let rest = bytes
        .strip_prefix(CRED_PREFIX)
        .context("credential identity: missing farder-mls-cred-v1 prefix")?;

    let (&pk_len, rest) = rest
        .split_first()
        .context("credential identity: truncated before pubkey length")?;
    if rest.len() < pk_len as usize {
        bail!("credential identity: truncated pubkey");
    }
    let (pk_bytes, rest) = rest.split_at(pk_len as usize);
    let pk_arr: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("credential identity: pubkey is not 32 bytes"))?;

    if rest.len() < 4 {
        bail!("credential identity: truncated before device id length");
    }
    let (len_bytes, rest) = rest.split_at(4);
    let dev_len = u32::from_be_bytes(len_bytes.try_into().expect("split_at(4)")) as usize;
    if rest.len() < dev_len {
        bail!("credential identity: truncated device id");
    }
    if rest.len() > dev_len {
        bail!("credential identity: trailing bytes after device id");
    }
    let device = String::from_utf8(rest.to_vec())
        .context("credential identity: device id is not valid UTF-8")?;

    Ok((PublicKey::from_bytes(pk_arr), device))
}

/// OpenMLS [`Signer`] adapter over a farder-crypto device subkey. The MLS
/// leaf signs with the same Ed25519 keypair that signs log events
/// (cross-protocol reuse judged safe: disjoint signed-byte domains, spec
/// §leaf keys).
pub struct DeviceSigner<'a>(pub &'a Keypair);

impl Signer for DeviceSigner<'_> {
    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
        Ok(self.0.sign(payload))
    }

    fn signature_scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

/// Build the [`CredentialWithKey`] for a device's MLS leaf: the normative
/// length-prefixed credential identity plus the device subkey as the leaf
/// signature key. Used for group creation (`MlsChannelGroup::create`) and
/// inside [`generate_key_package`].
pub fn credential_with_key(device: &Keypair, identity: &PublicKey) -> CredentialWithKey {
    let device_pubkey = device.public_key();
    let device_id = farder_crypto::event_log::device_id(&device_pubkey);
    let credential: Credential =
        BasicCredential::new(encode_credential_identity(identity, &device_id)).into();
    CredentialWithKey {
        credential,
        signature_key: device_pubkey.as_bytes().to_vec().into(),
    }
}

/// Generate a [`KeyPackageBundle`] for `device` under the pinned suite, with
/// the leaf credential bound to `(identity, device_id(device))` and the leaf
/// signature key set to the device subkey.
pub fn generate_key_package(
    provider: &impl OpenMlsProvider,
    device: &Keypair,
    identity: &PublicKey,
) -> Result<KeyPackageBundle> {
    let credential_with_key = credential_with_key(device, identity);
    KeyPackage::builder()
        .build(
            CIPHERSUITE,
            provider,
            &DeviceSigner(device),
            credential_with_key,
        )
        .map_err(|e| anyhow::anyhow!("generate key package: {e}"))
}

/// Verify the cryptographic binding credential ↔ leaf signature key ↔ an
/// identity-signed [`DeviceCert`]:
///
/// - the credential identity decodes and matches `cert.core.identity` /
///   `cert.core.device_id`,
/// - `leaf_signature_key` equals `cert.core.device_pubkey`,
/// - `cert.verify()` passes (identity signed this device subkey).
///
/// Fold-status checks (membership, revocation, expiry) are sub-2's job; the
/// caller must already trust `cert` per fold state.
pub fn verify_leaf_binding(
    credential: &Credential,
    leaf_signature_key: &[u8],
    cert: &DeviceCert,
) -> Result<()> {
    let (identity, device) = decode_credential_identity(credential.serialized_content())
        .context("leaf binding: credential identity does not decode")?;
    if identity != cert.core.identity {
        bail!("leaf binding: credential identity does not match device cert identity");
    }
    if device != cert.core.device_id {
        bail!("leaf binding: credential device id does not match device cert");
    }
    if leaf_signature_key != cert.core.device_pubkey.as_bytes() {
        bail!("leaf binding: leaf signature key does not match certified device subkey");
    }
    cert.verify()
        .context("leaf binding: device cert signature invalid")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::device_id;
    use openmls_rust_crypto::OpenMlsRustCrypto;

    fn keys() -> (Keypair, Keypair) {
        (Keypair::generate(), Keypair::generate())
    }

    #[test]
    fn credential_identity_roundtrips_via_length_prefixed_encoding() {
        let (identity, device) = keys();
        let dev_id = device_id(&device.public_key());
        let encoded = encode_credential_identity(&identity.public_key(), &dev_id);
        assert!(encoded.starts_with(b"farder-mls-cred-v1"));
        let (decoded_identity, decoded_device) = decode_credential_identity(&encoded).unwrap();
        assert_eq!(decoded_identity, identity.public_key());
        assert_eq!(decoded_device, dev_id);
    }

    #[test]
    fn credential_decoding_rejects_truncated_trailing_or_bad_prefix_bytes() {
        let (identity, device) = keys();
        let dev_id = device_id(&device.public_key());
        let encoded = encode_credential_identity(&identity.public_key(), &dev_id);

        // Every truncation of the valid encoding must fail.
        for cut in 0..encoded.len() {
            assert!(
                decode_credential_identity(&encoded[..cut]).is_err(),
                "truncation to {cut} bytes must be rejected"
            );
        }

        // One trailing byte must fail.
        let mut trailing = encoded.clone();
        trailing.push(0x00);
        assert!(decode_credential_identity(&trailing).is_err());

        // Wrong magic must fail.
        let mut bad_prefix = encoded.clone();
        bad_prefix[0] ^= 0xff;
        assert!(decode_credential_identity(&bad_prefix).is_err());

        // A pubkey length field that is not 32 must fail.
        let mut bad_len = encoded;
        bad_len[b"farder-mls-cred-v1".len()] = 31;
        assert!(decode_credential_identity(&bad_len).is_err());
    }

    #[test]
    fn key_package_is_signed_by_the_device_subkey_under_the_pinned_suite() {
        let (identity, device) = keys();
        let provider = OpenMlsRustCrypto::default();
        let bundle = generate_key_package(&provider, &device, &identity.public_key()).unwrap();
        let kp = bundle.key_package();
        assert_eq!(kp.ciphersuite(), crate::CIPHERSUITE);
        assert_eq!(
            kp.leaf_node().signature_key().as_slice(),
            device.public_key().as_bytes()
        );
        let (cred_identity, cred_device) =
            decode_credential_identity(kp.leaf_node().credential().serialized_content()).unwrap();
        assert_eq!(cred_identity, identity.public_key());
        assert_eq!(cred_device, device_id(&device.public_key()));
    }

    #[test]
    fn leaf_binding_accepts_a_matching_identity_signed_device_cert() {
        let (identity, device) = keys();
        let provider = OpenMlsRustCrypto::default();
        let bundle = generate_key_package(&provider, &device, &identity.public_key()).unwrap();
        let leaf = bundle.key_package().leaf_node();
        let cert = DeviceCert::create(&identity, &device.public_key(), 1_700_000_000);
        verify_leaf_binding(leaf.credential(), leaf.signature_key().as_slice(), &cert).unwrap();
    }

    #[test]
    fn leaf_binding_rejects_wrong_device_wrong_identity_or_tampered_cert() {
        let (identity, device) = keys();
        let provider = OpenMlsRustCrypto::default();
        let bundle = generate_key_package(&provider, &device, &identity.public_key()).unwrap();
        let leaf = bundle.key_package().leaf_node();
        let credential = leaf.credential();
        let leaf_key = leaf.signature_key().as_slice();

        // Cert for a different device of the same identity.
        let other_device = Keypair::generate();
        let wrong_device_cert = DeviceCert::create(&identity, &other_device.public_key(), 1);
        assert!(verify_leaf_binding(credential, leaf_key, &wrong_device_cert).is_err());

        // Cert from a different identity over the same device subkey.
        let other_identity = Keypair::generate();
        let wrong_identity_cert = DeviceCert::create(&other_identity, &device.public_key(), 1);
        assert!(verify_leaf_binding(credential, leaf_key, &wrong_identity_cert).is_err());

        // Tampered cert core: signature no longer verifies.
        let mut tampered = DeviceCert::create(&identity, &device.public_key(), 1);
        tampered.core.created_at = 2;
        assert!(verify_leaf_binding(credential, leaf_key, &tampered).is_err());
    }
}
