//! `farder-mls` — pure-Rust wrapper around OpenMLS 0.8.1 for Farder's E2EE
//! channel groups (mesh rung 2, sub-project 1).
//!
//! Client-side only: the server never links this crate and never holds group
//! keys. See `docs/modules/farder-mls.md` and the rung-2 E2EE design spec.

use openmls::prelude::Ciphersuite;

/// The one ciphersuite Farder groups use: RFC 9420's MTI suite
/// (X25519 HPKE, AES-128-GCM, SHA-256, Ed25519 signatures).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Padding bucket ladder (bytes) applied to the plaintext envelope before
/// sealing, so ciphertext length is not a plaintext-length oracle. Envelopes
/// larger than the top bucket are refused, never truncated.
pub const PADDING_BUCKETS: [usize; 5] = [256, 1024, 4096, 16384, 40960];

/// How many past epochs' secrets a member retains for late-arriving
/// application messages (spec: `max_past_epochs = 3`).
pub const MAX_PAST_EPOCHS: usize = 3;

/// Client-side pre-seal cap on the encoded envelope (spec Size-caps row:
/// <= 8000 chars AND <= 32 KiB pre-seal; the 40 KiB ciphertext cap is
/// ingest's job, sub-3).
pub const MAX_PRESEAL_BYTES: usize = 32 * 1024;

/// Client-side cap on message content length in characters.
pub const MAX_CONTENT_CHARS: usize = 8000;

#[cfg(test)]
mod tests {
    use super::*;
    use openmls::prelude::*;
    use openmls_rust_crypto::OpenMlsRustCrypto;
    use openmls_traits::OpenMlsProvider;

    #[test]
    fn pinned_openmls_stack_supports_the_mti_ciphersuite() {
        let provider = OpenMlsRustCrypto::default();
        assert!(provider.crypto().supports(CIPHERSUITE).is_ok());
        assert_eq!(CIPHERSUITE.signature_algorithm(), SignatureScheme::ED25519);
    }

    #[test]
    fn padding_ladder_is_strictly_increasing_and_caps_at_40kib() {
        assert!(PADDING_BUCKETS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(*PADDING_BUCKETS.last().unwrap(), 40 * 1024);
        assert!(MAX_PRESEAL_BYTES < *PADDING_BUCKETS.last().unwrap());
    }
}
