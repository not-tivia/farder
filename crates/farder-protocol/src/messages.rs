use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    RelayConnect { destination_id: Vec<u8> },
    RelayConnected,
    RelayError { reason: String },
    KeyExchange { sender: PublicKey, session_public_key: [u8; 32] },
    KeyExchangeResponse { responder: PublicKey, session_public_key: [u8; 32] },
    EncryptedDm { sender: PublicKey, ciphertext: Vec<u8>, timestamp: u64 },
    NotifyRegister { public_key: PublicKey },
    NotifyPending { count: u32 },
    NotifyFetch,
    NotifyMessages { messages: Vec<QueuedMessage> },
    NotifyDeliver { recipient: PublicKey, payload: Vec<u8> },
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
