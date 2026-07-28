//! The sealed message body and the padding bucket ladder.
//!
//! [`MessageEnvelope`] is the exact plaintext structure sealed inside a
//! `MessagePostedE2ee` ciphertext (spec §Wire formats): message content plus
//! the in-band per-file attachment keys, real filenames, and real MIME types
//! that must never appear outside the seal. It serializes to canonical
//! rmp-serde bytes ([`MessageEnvelope::encode`] / [`MessageEnvelope::decode`]).
//!
//! Before sealing, the encoded envelope is padded to the
//! [`crate::PADDING_BUCKETS`] ladder ([`pad_to_bucket`]) so ciphertext length
//! is not a plaintext-length oracle; envelopes above the top bucket are
//! **refused, never truncated**. [`check_preseal_limits`] enforces the
//! client-side half of the spec's size caps (<= 8000 chars AND <= 32 KiB
//! pre-seal); the 40 KiB ciphertext cap is ingest's job (sub-3).
//!
//! Padding layout: `u32_be(len) || bytes || 0x00…` sized to the smallest
//! bucket >= `len + 4`. [`unpad`] is strict: the total length must be exactly
//! one of the buckets and the prefix must fit; the zero tail is not checked
//! (padding sits inside the AEAD — integrity comes from MLS, not the tail).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{MAX_CONTENT_CHARS, MAX_PRESEAL_BYTES, PADDING_BUCKETS};

/// The plaintext body of a sealed channel message.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Default)]
pub struct MessageEnvelope {
    /// The message text.
    pub content: String,
    /// Random per-file AES-256-GCM keys for this message's attachments.
    pub attachment_keys: Vec<[u8; 32]>,
    /// Real filenames of the attachments (attacker-controlled on receipt;
    /// sanitization before write/render is the client's job, sub-6).
    pub filenames: Vec<String>,
    /// Real MIME types of the attachments.
    pub mimes: Vec<String>,
}

impl MessageEnvelope {
    /// Canonical rmp-serde bytes — what gets padded and sealed.
    pub fn encode(&self) -> Result<Vec<u8>> {
        rmp_serde::to_vec(self).context("encode message envelope")
    }

    /// Strict decode of unpadded envelope bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("decode message envelope")
    }
}

/// Pad encoded envelope bytes to the smallest [`PADDING_BUCKETS`] entry that
/// fits `u32_be(len) || bytes`; zero-filled tail. Errs (never truncates) if
/// the payload exceeds the top bucket.
pub fn pad_to_bucket(bytes: &[u8]) -> Result<Vec<u8>> {
    let needed = bytes.len() + 4;
    let Some(bucket) = PADDING_BUCKETS.iter().copied().find(|&b| b >= needed) else {
        bail!(
            "payload of {} bytes exceeds the top padding bucket ({} bytes); refusing to seal",
            bytes.len(),
            PADDING_BUCKETS[PADDING_BUCKETS.len() - 1]
        );
    };
    let mut padded = vec![0u8; bucket];
    padded[..4].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    padded[4..needed].copy_from_slice(bytes);
    Ok(padded)
}

/// Strictly reverse [`pad_to_bucket`]: total length must be exactly one of
/// the buckets and the length prefix must fit inside it.
pub fn unpad(padded: &[u8]) -> Result<Vec<u8>> {
    if !PADDING_BUCKETS.contains(&padded.len()) {
        bail!(
            "padded length {} is not a padding bucket size",
            padded.len()
        );
    }
    let len_prefix: [u8; 4] = padded[..4].try_into().expect("buckets are all >= 4 bytes");
    let len = u32::from_be_bytes(len_prefix) as usize;
    if len > padded.len() - 4 {
        bail!(
            "length prefix {len} does not fit inside the {}-byte bucket",
            padded.len()
        );
    }
    Ok(padded[4..4 + len].to_vec())
}

/// The client-rule half of the spec's size caps: content <= 8000 chars AND
/// encoded envelope <= 32 KiB pre-seal.
pub fn check_preseal_limits(envelope: &MessageEnvelope, encoded_len: usize) -> Result<()> {
    let chars = envelope.content.chars().count();
    if chars > MAX_CONTENT_CHARS {
        bail!("message content is {chars} chars, over the {MAX_CONTENT_CHARS}-char cap");
    }
    if encoded_len > MAX_PRESEAL_BYTES {
        bail!(
            "encoded envelope is {encoded_len} bytes, over the {MAX_PRESEAL_BYTES}-byte pre-seal cap"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{credential_with_key, generate_key_package, DeviceSigner};
    use crate::group::MlsChannelGroup;
    use farder_crypto::identity::Keypair;
    use openmls_rust_crypto::OpenMlsRustCrypto;

    /// One in-memory test member: identity + device keys, provider, group.
    struct TestMember {
        identity: Keypair,
        device: Keypair,
        provider: OpenMlsRustCrypto,
        group: MlsChannelGroup,
    }

    impl TestMember {
        fn seal(&mut self, envelope: &MessageEnvelope) -> Result<Vec<u8>> {
            let signer = DeviceSigner(&self.device);
            self.group.seal_message(&self.provider, &signer, envelope)
        }

        fn open(&mut self, bytes: &[u8]) -> Result<MessageEnvelope> {
            self.group.open_message(&self.provider, bytes)
        }
    }

    /// Founder creates the group and adds `n - 1` members in one commit; the
    /// added members join from the shared Welcome. Returns founder first.
    fn group_of(n: usize, group_id: &[u8]) -> Vec<TestMember> {
        let founder_identity = Keypair::generate();
        let founder_device = Keypair::generate();
        let founder_provider = OpenMlsRustCrypto::default();
        let founder_group = MlsChannelGroup::create(
            &founder_provider,
            &DeviceSigner(&founder_device),
            credential_with_key(&founder_device, &founder_identity.public_key()),
            group_id,
        )
        .unwrap();
        let mut founder = TestMember {
            identity: founder_identity,
            device: founder_device,
            provider: founder_provider,
            group: founder_group,
        };

        let mut joiners: Vec<(Keypair, Keypair, OpenMlsRustCrypto)> = Vec::new();
        let mut key_packages = Vec::new();
        for _ in 1..n {
            let identity = Keypair::generate();
            let device = Keypair::generate();
            let provider = OpenMlsRustCrypto::default();
            let bundle = generate_key_package(&provider, &device, &identity.public_key()).unwrap();
            key_packages.push(bundle.key_package().clone());
            joiners.push((identity, device, provider));
        }

        let mut members = Vec::with_capacity(n);
        if key_packages.is_empty() {
            members.push(founder);
            return members;
        }
        let signer = DeviceSigner(&founder.device);
        let outcome = founder
            .group
            .add_members(&founder.provider, &signer, &key_packages)
            .unwrap();
        let welcome = outcome.welcome_bytes.unwrap();
        members.push(founder);
        for (identity, device, provider) in joiners {
            let (group, _) = MlsChannelGroup::join_from_welcome(&provider, &welcome).unwrap();
            members.push(TestMember {
                identity,
                device,
                provider,
                group,
            });
        }
        members
    }

    fn envelope_with_attachment(content: &str) -> MessageEnvelope {
        MessageEnvelope {
            content: content.to_string(),
            attachment_keys: vec![[0xAB; 32]],
            filenames: vec!["secret-blueprints.pdf".to_string()],
            mimes: vec!["application/pdf".to_string()],
        }
    }

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn padded_sizes_land_exactly_on_the_bucket_ladder() {
        // (payload len, expected bucket): the +4 length prefix decides the
        // boundary — 252 + 4 == 256 still fits, 253 + 4 spills to 1024.
        let cases = [
            (0usize, 256usize),
            (251, 256),
            (252, 256),
            (253, 1024),
            (1020, 1024),
            (1021, 4096),
            (4092, 4096),
            (16380, 16384),
            (40956, 40960),
        ];
        for (len, bucket) in cases {
            let payload = vec![0x5A; len];
            let padded = pad_to_bucket(&payload).unwrap();
            assert_eq!(padded.len(), bucket, "payload of {len} bytes");
            assert_eq!(unpad(&padded).unwrap(), payload, "payload of {len} bytes");
        }
        // Over the top bucket: refused, never truncated.
        assert!(pad_to_bucket(&vec![0x5A; 40957]).is_err());
        assert!(pad_to_bucket(&vec![0x5A; 100_000]).is_err());
        // Strict unpad: non-bucket total lengths and lying prefixes are refused.
        assert!(unpad(&[0u8; 255]).is_err());
        assert!(unpad(&[0u8; 257]).is_err());
        assert!(unpad(&[0u8; 3]).is_err());
        let mut lying = pad_to_bucket(b"hi").unwrap();
        lying[..4].copy_from_slice(&300u32.to_be_bytes()); // claims more than the bucket holds
        assert!(unpad(&lying).is_err());
    }

    #[test]
    fn oversize_content_is_refused_not_truncated() {
        let mut members = group_of(2, b"oversize-test");
        let mut alice = members.remove(0);

        // 8001 ASCII chars: over the char cap.
        let too_many_chars = MessageEnvelope {
            content: "a".repeat(MAX_CONTENT_CHARS + 1),
            ..Default::default()
        };
        assert!(alice.seal(&too_many_chars).is_err());
        // The envelope is untouched — refused, not truncated.
        assert_eq!(too_many_chars.content.chars().count(), MAX_CONTENT_CHARS + 1);

        // Exactly 8000 chars of 4-byte UTF-8 (32 000 bytes) passes the char
        // rule, but a bulky filename pushes the encoded envelope past 32 KiB.
        let over_preseal_bytes = MessageEnvelope {
            content: "\u{1F600}".repeat(MAX_CONTENT_CHARS),
            attachment_keys: vec![[7; 32]],
            filenames: vec!["f".repeat(2048)],
            mimes: vec!["application/octet-stream".to_string()],
        };
        assert_eq!(
            over_preseal_bytes.content.chars().count(),
            MAX_CONTENT_CHARS
        );
        assert!(over_preseal_bytes.encode().unwrap().len() > MAX_PRESEAL_BYTES);
        assert!(alice.seal(&over_preseal_bytes).is_err());

        // check_preseal_limits is the enforcer for both halves.
        assert!(check_preseal_limits(&too_many_chars, 100).is_err());
        assert!(check_preseal_limits(&MessageEnvelope::default(), MAX_PRESEAL_BYTES + 1).is_err());
        assert!(check_preseal_limits(&MessageEnvelope::default(), MAX_PRESEAL_BYTES).is_ok());
    }

    #[test]
    fn sealed_message_roundtrips_between_members() {
        let mut members = group_of(2, b"roundtrip-test");
        let mut bob = members.remove(1);
        let mut alice = members.remove(0);

        let envelope = envelope_with_attachment("hello over the sealed channel");
        let sealed = alice.seal(&envelope).unwrap();
        let opened = bob.open(&sealed).unwrap();
        assert_eq!(opened, envelope);
    }

    #[test]
    fn sealed_bytes_contain_no_plaintext_substring() {
        // OBSERVATION (CLAUDE.md): assert on the bytes that actually go to
        // the wire, not on the code path.
        let mut members = group_of(2, b"observation-test");
        let mut alice = members.remove(0);

        let envelope =
            envelope_with_attachment("TOP-SECRET-PLAINTEXT-MARKER do not leak this sentence");
        let sealed = alice.seal(&envelope).unwrap();

        assert!(!contains_subslice(&sealed, envelope.content.as_bytes()));
        assert!(!contains_subslice(&sealed, b"TOP-SECRET-PLAINTEXT-MARKER"));
        assert!(!contains_subslice(&sealed, envelope.filenames[0].as_bytes()));
        assert!(!contains_subslice(&sealed, envelope.mimes[0].as_bytes()));
        assert!(!contains_subslice(&sealed, &envelope.attachment_keys[0]));
        // And the whole encoded envelope, for good measure.
        assert!(!contains_subslice(&sealed, &envelope.encode().unwrap()));
    }

    #[test]
    fn two_envelopes_in_the_same_bucket_seal_to_equal_length_ciphertexts() {
        // The length-oracle fix: "yes" and a 200-char paragraph land in the
        // same 256-byte bucket, so their ciphertexts are the same length.
        let mut members = group_of(2, b"length-oracle-test");
        let mut alice = members.remove(0);

        let short = MessageEnvelope {
            content: "yes".to_string(),
            ..Default::default()
        };
        let long = MessageEnvelope {
            content: "no, and here is a two-hundred-character explanation of why not: "
                .chars()
                .cycle()
                .take(200)
                .collect(),
            ..Default::default()
        };
        assert_eq!(long.content.chars().count(), 200);

        let sealed_short = alice.seal(&short).unwrap();
        let sealed_long = alice.seal(&long).unwrap();
        assert_eq!(sealed_short.len(), sealed_long.len());
    }

    #[test]
    fn removed_member_cannot_decrypt_post_removal() {
        // FS OBSERVATION: after carol's removal is merged, a message sealed
        // in the new epoch is unreadable to her.
        let mut members = group_of(3, b"fs-removal-test");
        let mut carol = members.remove(2);
        let mut bob = members.remove(1);
        let mut alice = members.remove(0);

        let carol_declared = crate::group::DeclaredMember {
            identity: carol.identity.public_key(),
            device: farder_crypto::event_log::device_id(&carol.device.public_key()),
        };
        let signer = DeviceSigner(&alice.device);
        let outcome = alice
            .group
            .remove_members(&alice.provider, &signer, &[carol_declared])
            .unwrap();
        bob.group
            .process_commit(&bob.provider, &outcome.commit_bytes)
            .unwrap();
        carol
            .group
            .process_commit(&carol.provider, &outcome.commit_bytes)
            .unwrap();

        let envelope = envelope_with_attachment("carol must not read this");
        let sealed = alice.seal(&envelope).unwrap();
        assert_eq!(bob.open(&sealed).unwrap(), envelope);
        assert!(carol.open(&sealed).is_err());
    }

    #[test]
    fn joiner_cannot_decrypt_pre_join_messages() {
        // FS OBSERVATION: carol joins after the message epoch and holds no
        // secrets for it.
        let mut members = group_of(2, b"fs-join-test");
        let mut bob = members.remove(1);
        let mut alice = members.remove(0);

        let envelope = envelope_with_attachment("history carol never gets");
        let pre_join_sealed = alice.seal(&envelope).unwrap();
        assert_eq!(bob.open(&pre_join_sealed).unwrap(), envelope);

        let carol_identity = Keypair::generate();
        let carol_device = Keypair::generate();
        let carol_provider = OpenMlsRustCrypto::default();
        let bundle =
            generate_key_package(&carol_provider, &carol_device, &carol_identity.public_key())
                .unwrap();
        let signer = DeviceSigner(&alice.device);
        let outcome = alice
            .group
            .add_members(&alice.provider, &signer, &[bundle.key_package().clone()])
            .unwrap();
        bob.group
            .process_commit(&bob.provider, &outcome.commit_bytes)
            .unwrap();
        let (carol_group, _) =
            MlsChannelGroup::join_from_welcome(&carol_provider, &outcome.welcome_bytes.unwrap())
                .unwrap();
        let mut carol = TestMember {
            identity: carol_identity,
            device: carol_device,
            provider: carol_provider,
            group: carol_group,
        };

        assert!(carol.open(&pre_join_sealed).is_err());
        // Sanity: carol reads the epoch she was added into.
        let post = envelope_with_attachment("carol reads this one");
        let sealed_post = alice.seal(&post).unwrap();
        assert_eq!(carol.open(&sealed_post).unwrap(), post);
    }

    #[test]
    fn tampered_ciphertext_and_wrong_group_fail_to_open() {
        let mut members = group_of(2, b"tamper-test");
        let mut bob = members.remove(1);
        let mut alice = members.remove(0);

        let envelope = envelope_with_attachment("integrity matters");
        let sealed = alice.seal(&envelope).unwrap();

        // A member of a completely unrelated group cannot open it.
        let mut other = group_of(2, b"some-other-group");
        let mut dave = other.remove(0);
        assert!(dave.open(&sealed).is_err());

        // Bit-flip inside the ciphertext body: AEAD must reject. OpenMLS
        // 0.8.1 has `debug_assert!(false, ..)` on AEAD failure
        // (framing/private_message_in.rs), so debug builds (cargo test)
        // panic where release builds return Err — accept either
        // "did not open" outcome. See docs/modules/farder-mls.md gotchas.
        let mut tampered = sealed.clone();
        let mid = tampered.len() - 10;
        tampered[mid] ^= 0x01;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bob.open(&tampered)));
        assert!(
            !matches!(result, Ok(Ok(_))),
            "tampered ciphertext must not open"
        );

        // Bob's ratchet is intact: a fresh sealed message still opens. (The
        // tampered attempt consumed that generation's decryption key — a
        // forward-secrecy property, not damage — so the sanity check uses a
        // new message rather than re-opening `sealed`.)
        let follow_up = envelope_with_attachment("still sealed, still working");
        let sealed_follow_up = alice.seal(&follow_up).unwrap();
        assert_eq!(bob.open(&sealed_follow_up).unwrap(), follow_up);
    }
}
