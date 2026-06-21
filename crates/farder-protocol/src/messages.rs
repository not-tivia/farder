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
    /// A valid invite code: the server responded with its name and counts.
    /// `member_count` is the total number of registered members;
    /// `online_count` is the number of members currently connected.
    Preview { server_name: String, member_count: u32, online_count: u32 },
    /// The server answered: the code is invalid/expired/exhausted. Uniform on
    /// purpose — invalid codes reveal nothing about the server.
    Invalid,
    /// Timeout, dial failure, SSRF refusal, rate-limit refusal, or an
    /// undecodable answer.
    Unavailable,
}

/// Coarse class of an external embed, used by the client to pick a card layout.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum EmbedKind {
    Tweet,
    Video,
    Image,
    Audio,
    Article,
}

/// A directly-fetchable media asset (image or direct video file) the client
/// renders inline by pulling its bytes via `ProxyMedia`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EmbedMedia {
    pub url: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// true for direct-file media playable in a `<video>`/`<img>`; false for
    /// sources (YouTube, Spotify) that must open in an external browser.
    pub playable_inline: bool,
}

/// Normalized metadata for one external link, produced by a relay-side adapter.
/// The client never sees raw HTML; only this struct.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LinkEmbed {
    pub provider: String,
    pub kind: EmbedKind,
    pub url: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    /// URL of a thumbnail/preview image, fetched via `ProxyMedia`.
    pub thumbnail: Option<String>,
    pub media: Option<EmbedMedia>,
    pub duration_secs: Option<u32>,
}

/// Result of a `ProxyLinkEmbed` lookup. Uniform failure leaks nothing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum EmbedOutcome {
    Embed(LinkEmbed),
    /// URL host is allowlisted but the specific URL shape isn't handled.
    Unsupported,
    /// Timeout, SSRF refusal, non-allowlisted host, rate-limit, parse failure.
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
    /// The relay's answer to a `ProxyInvitePreview` request. Sent by the relay
    /// after resolving the invite code against the target server.
    ProxyInvitePreviewResult { outcome: PreviewOutcome },
    /// Ask the relay to resolve a rich embed for an external URL (relay fetch
    /// proxy, phase two). First message on a fresh connection.
    ProxyLinkEmbed { url: String },
    /// The relay's normalized answer to `ProxyLinkEmbed`.
    ProxyLinkEmbedResult { outcome: EmbedOutcome },
    /// Ask the relay to stream a media/thumbnail asset (image or direct video)
    /// on the requester's behalf. First message on a fresh connection.
    ProxyMedia { url: String },
    /// Sent by the relay before the raw media bytes: the validated content type
    /// and total length. Followed by length-framed raw chunks on the stream.
    ProxyMediaHeader { content_type: String, total_len: u64 },
    /// Sent by the relay instead of a header when the media can't be served
    /// (non-allowlisted, SSRF refusal, over cap, bad content-type, timeout).
    ProxyMediaUnavailable,
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

    #[test]
    fn test_roundtrip_link_embed() {
        let embed = LinkEmbed {
            provider: "twitter".into(),
            kind: EmbedKind::Tweet,
            url: "https://x.com/a/status/1".into(),
            title: Some("hi".into()),
            author: Some("@a".into()),
            description: Some("body".into()),
            thumbnail: Some("https://pbs.example/t.jpg".into()),
            media: Some(EmbedMedia {
                url: "https://video.example/v.mp4".into(),
                mime: "video/mp4".into(),
                width: Some(640),
                height: Some(360),
                playable_inline: true,
            }),
            duration_secs: Some(12),
        };
        for outcome in [
            EmbedOutcome::Embed(embed.clone()),
            EmbedOutcome::Unsupported,
            EmbedOutcome::Unavailable,
        ] {
            let msg = Message::ProxyLinkEmbedResult { outcome: outcome.clone() };
            let bytes = codec::encode(&msg).unwrap();
            match codec::decode::<Message>(&bytes).unwrap() {
                Message::ProxyLinkEmbedResult { outcome: o } => assert_eq!(o, outcome),
                other => panic!("wrong variant: {other:?}"),
            }
        }

        let req = Message::ProxyLinkEmbed { url: "https://x.com/a/status/1".into() };
        assert!(matches!(
            codec::decode::<Message>(&codec::encode(&req).unwrap()).unwrap(),
            Message::ProxyLinkEmbed { .. }
        ));

        let media = Message::ProxyMedia { url: "https://video.example/v.mp4".into() };
        assert!(matches!(
            codec::decode::<Message>(&codec::encode(&media).unwrap()).unwrap(),
            Message::ProxyMedia { .. }
        ));

        let hdr = Message::ProxyMediaHeader { content_type: "image/jpeg".into(), total_len: 1024 };
        assert!(matches!(
            codec::decode::<Message>(&codec::encode(&hdr).unwrap()).unwrap(),
            Message::ProxyMediaHeader { .. }
        ));
    }
}
