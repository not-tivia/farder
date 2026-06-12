use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

/// What the relay should query for an invite preview.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PreviewTarget {
    /// A server registered with THIS relay (relayed server).
    Registered { server_id: Vec<u8> },
    /// A direct server the relay should dial on the requester's behalf.
    Direct { addr: String },
}

/// Result of an invite-preview lookup, as relayed back to the requester.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PreviewOutcome {
    Preview { server_name: String, member_count: u32, online_count: u32 },
    /// The server answered: the code is invalid/expired/exhausted. Uniform on
    /// purpose — invalid codes reveal nothing about the server.
    Invalid,
    /// Timeout, dial failure, SSRF refusal, rate-limit refusal, or an
    /// undecodable answer.
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    RelayConnect { destination_id: Vec<u8> },
    RelayConnected,
    RelayError { reason: String },
    RelayRegister { server_id: Vec<u8> },
    RelayRegistered,
    KeyExchange { sender: PublicKey, session_public_key: [u8; 32] },
    KeyExchangeResponse { responder: PublicKey, session_public_key: [u8; 32] },
    EncryptedDm { sender: PublicKey, ciphertext: Vec<u8>, timestamp: u64 },
    NotifyRegister { public_key: PublicKey },
    NotifyPending { count: u32 },
    NotifyFetch,
    NotifyMessages { messages: Vec<QueuedMessage> },
    NotifyDeliver { recipient: PublicKey, payload: Vec<u8> },
    DmFileHeader { sender: PublicKey, encrypted_header: Vec<u8> },
    DmFileChunk { sender: PublicKey, encrypted_chunk: Vec<u8> },
    DmFileComplete { sender: PublicKey },
    /// Ask the relay to fetch an invite preview on the requester's behalf
    /// (relay fetch proxy, phase one). First message on a fresh connection.
    ProxyInvitePreview { target: PreviewTarget, code: String },
    ProxyInvitePreviewResult { outcome: PreviewOutcome },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub sender: PublicKey,
    pub payload: Vec<u8>,
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;
    use farder_crypto::identity::Keypair;

    #[test]
    fn test_roundtrip_relay_connect() {
        let destination_id = vec![1u8, 2, 3, 4, 5];
        let msg = Message::RelayConnect { destination_id: destination_id.clone() };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::RelayConnect { destination_id: decoded_id } => {
                assert_eq!(destination_id, decoded_id);
            }
            _ => panic!("wrong variant after decode"),
        }
    }

    #[test]
    fn test_roundtrip_encrypted_dm() {
        let keypair = Keypair::generate();
        let sender = keypair.public_key();
        let ciphertext = vec![10u8, 20, 30, 40];
        let timestamp = 1_700_000_000u64;
        let msg = Message::EncryptedDm {
            sender: sender.clone(),
            ciphertext: ciphertext.clone(),
            timestamp,
        };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::EncryptedDm { sender: s, ciphertext: c, timestamp: t } => {
                assert_eq!(sender.as_bytes(), s.as_bytes());
                assert_eq!(ciphertext, c);
                assert_eq!(timestamp, t);
            }
            _ => panic!("wrong variant after decode"),
        }
    }

    #[test]
    fn test_roundtrip_dm_file_header() {
        let kp = Keypair::generate();
        let msg = Message::DmFileHeader {
            sender: kp.public_key(),
            encrypted_header: vec![1, 2, 3, 4],
        };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::DmFileHeader { sender, encrypted_header } => {
                assert_eq!(sender.as_bytes(), kp.public_key().as_bytes());
                assert_eq!(encrypted_header, vec![1, 2, 3, 4]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_relay_register() {
        let msg = Message::RelayRegister { server_id: vec![9u8, 8, 7, 6] };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::RelayRegister { server_id } => assert_eq!(server_id, vec![9u8, 8, 7, 6]),
            other => panic!("expected RelayRegister, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_relay_registered() {
        let encoded = codec::encode(&Message::RelayRegistered).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        assert!(matches!(decoded, Message::RelayRegistered));
    }

    #[test]
    fn test_roundtrip_proxy_invite_preview() {
        let msg = Message::ProxyInvitePreview {
            target: PreviewTarget::Registered { server_id: vec![1u8; 32] },
            code: "AbCd1234".to_string(),
        };
        let encoded = codec::encode(&msg).expect("encode failed");
        match codec::decode::<Message>(&encoded).expect("decode failed") {
            Message::ProxyInvitePreview { target: PreviewTarget::Registered { server_id }, code } => {
                assert_eq!(server_id, vec![1u8; 32]);
                assert_eq!(code, "AbCd1234");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let msg = Message::ProxyInvitePreview {
            target: PreviewTarget::Direct { addr: "203.0.113.7:4433".to_string() },
            code: "x".to_string(),
        };
        let encoded = codec::encode(&msg).unwrap();
        assert!(matches!(
            codec::decode::<Message>(&encoded).unwrap(),
            Message::ProxyInvitePreview { target: PreviewTarget::Direct { .. }, .. }
        ));

        for outcome in [
            PreviewOutcome::Preview { server_name: "The Spot".into(), member_count: 12, online_count: 3 },
            PreviewOutcome::Invalid,
            PreviewOutcome::Unavailable,
        ] {
            let msg = Message::ProxyInvitePreviewResult { outcome: outcome.clone() };
            let encoded = codec::encode(&msg).unwrap();
            match codec::decode::<Message>(&encoded).unwrap() {
                Message::ProxyInvitePreviewResult { outcome: o } => assert_eq!(o, outcome),
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn test_roundtrip_key_exchange() {
        let keypair = Keypair::generate();
        let sender = keypair.public_key();
        let session_public_key = [42u8; 32];
        let msg = Message::KeyExchange {
            sender: sender.clone(),
            session_public_key,
        };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::KeyExchange { sender: s, session_public_key: spk } => {
                assert_eq!(sender.as_bytes(), s.as_bytes());
                assert_eq!(session_public_key, spk);
            }
            _ => panic!("wrong variant after decode"),
        }
    }
}
