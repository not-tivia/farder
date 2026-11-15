//! Farder's real protocol and signing, for the browser guest page (docs/web-guest-join-design.md,
//! step 3). The same Rust that the app and server use, compiled to WebAssembly, so the page cannot
//! drift from the wire format and the guest's key is made and kept in the browser.
//!
//! The page deals in two things only: JSON text (readable requests and answers) and bytes (what goes
//! over the WebSocket, one message per frame). Everything between is here.
//!
//! The logic is plain Rust functions (tested natively); the `#[wasm_bindgen]` layer is a thin wrapper.

use farder_crypto::event_log::{DeviceCert, Event, EventPayload};
use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerFrame, ServerRequest},
};
use wasm_bindgen::prelude::*;

/// A throwaway guest identity.
#[wasm_bindgen]
pub struct GuestKey {
    kp: Keypair,
    /// The first event of this guest's chain, kept between the two join steps.
    first: Option<Event>,
}

#[wasm_bindgen]
impl GuestKey {
    /// A new random key (from the browser's secure random source).
    #[wasm_bindgen(constructor)]
    pub fn new() -> GuestKey {
        GuestKey { kp: Keypair::generate(), first: None }
    }

    /// Rebuild a key the page saved earlier (so a refresh does not log the guest out).
    pub fn from_secret(secret: &[u8]) -> Result<GuestKey, JsError> {
        let arr: [u8; 32] = secret.try_into().map_err(|_| JsError::new("a key is 32 bytes"))?;
        Ok(GuestKey { kp: Keypair::from_signing_key_bytes(&arr), first: None })
    }

    /// The secret, for the page to keep in the browser's own storage. Never send it anywhere.
    pub fn secret(&self) -> Vec<u8> {
        self.kp.signing_key_bytes().to_vec()
    }

    /// The public key as the app shows it (`vk_` and 64 hex characters).
    pub fn public_key(&self) -> String {
        self.kp.public_key().to_string()
    }

    /// Join step 1 on a server with a signed history: the request frame that authorizes this guest's
    /// device (a key made from the guest's own, so only one secret is kept). `now` is seconds since 1970.
    pub fn begin_join(&mut self, id: u32, log_server_id: &str, now: f64) -> Result<Vec<u8>, JsError> {
        let (ev, bytes) = begin_join(&self.kp, id, log_server_id, now as u64).map_err(|e| JsError::new(&e))?;
        self.first = Some(ev);
        Ok(bytes)
    }

    /// Join step 2: the request frame saying "I joined with this invite" (`invite_event` is what the
    /// server answered to ResolveInvite). Call after step 1 was accepted.
    pub fn finish_join(&self, id: u32, log_server_id: &str, invite_event: &str, now: f64) -> Result<Vec<u8>, JsError> {
        let first = self.first.as_ref().ok_or_else(|| JsError::new("call begin_join first"))?;
        finish_join(&self.kp, id, log_server_id, invite_event, first, now as u64).map_err(|e| JsError::new(&e))
    }

    /// The request frame that sets this guest's display name (signed by the guest's key).
    pub fn profile_request(&self, id: u32, display_name: &str) -> Result<Vec<u8>, JsError> {
        let profile = farder_crypto::profile::SignedProfile::create(&self.kp, display_name.to_string(), None, None).to_bytes();
        codec::encode(&ClientFrame::Request { id, body: ServerRequest::UpdateProfile { profile } }).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Answer the server's Challenge frame: returns the Authenticate frame to send. The guest's own
    /// key signs it. `invite` is the guest link code (empty for a returning guest).
    pub fn answer_challenge(&self, challenge_frame: &[u8], invite: &str) -> Result<Vec<u8>, JsError> {
        answer_challenge(&self.kp, challenge_frame, invite).map_err(|e| JsError::new(&e))
    }
}

/// Turn a request written as JSON (for example `{"SendMessage":{"channel_id":1,"content":"hi",
/// "reply_to":null,"attachment_ids":[]}}` or `"GetServerInfo"`) into the bytes for one request frame.
#[wasm_bindgen]
pub fn encode_request(id: u32, request_json: &str) -> Result<Vec<u8>, JsError> {
    encode_request_impl(id, request_json).map_err(|e| JsError::new(&e))
}

/// Turn one frame from the server into readable JSON text.
#[wasm_bindgen]
pub fn decode_server_frame(bytes: &[u8]) -> Result<String, JsError> {
    decode_server_frame_impl(bytes).map_err(|e| JsError::new(&e))
}

// ---- plain Rust, tested natively ----

/// The guest's device key is derived from the guest's own with SHA-256 and a fixed label, so the page
/// keeps one secret, the device key is stable across refreshes, and knowing the device key says
/// nothing about the identity key.
fn device_key(identity: &Keypair) -> Keypair {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"farder-guest-device-v1");
    h.update(identity.signing_key_bytes());
    let out: [u8; 32] = h.finalize().into();
    Keypair::from_signing_key_bytes(&out)
}

pub fn begin_join(kp: &Keypair, id: u32, log_server_id: &str, now: u64) -> Result<(Event, Vec<u8>), String> {
    let dev = device_key(kp);
    let cert = DeviceCert::create(kp, &dev.public_key(), now);
    let ev = Event::next(&dev, kp.public_key(), log_server_id.to_string(), None, 0, now, EventPayload::DeviceAuthorized { cert });
    let bytes = codec::encode(&ClientFrame::Request { id, body: ServerRequest::SubmitEvent { event: ev.clone() } }).map_err(|e| e.to_string())?;
    Ok((ev, bytes))
}

pub fn finish_join(kp: &Keypair, id: u32, log_server_id: &str, invite_event: &str, first: &Event, now: u64) -> Result<Vec<u8>, String> {
    let dev = device_key(kp);
    let ev = Event::next(&dev, kp.public_key(), log_server_id.to_string(), Some(first), first.core.lamport, now,
        EventPayload::MemberJoined { member: kp.public_key(), invite: invite_event.to_string() });
    codec::encode(&ClientFrame::Request { id, body: ServerRequest::SubmitEvent { event: ev } }).map_err(|e| e.to_string())
}

pub fn answer_challenge(kp: &Keypair, challenge_frame: &[u8], invite: &str) -> Result<Vec<u8>, String> {
    let frame: ServerFrame = codec::decode(challenge_frame).map_err(|e| format!("not a server frame: {e}"))?;
    let ServerFrame::Challenge { nonce } = frame else { return Err("that frame is not a Challenge".into()) };
    let auth = ClientFrame::Authenticate {
        public_key: kp.public_key(),
        signed_challenge: kp.sign(&nonce),
        invite_code: if invite.is_empty() { None } else { Some(invite.to_string()) },
        setup_token: None,
    };
    codec::encode(&auth).map_err(|e| e.to_string())
}

pub fn encode_request_impl(id: u32, request_json: &str) -> Result<Vec<u8>, String> {
    let body: ServerRequest = serde_json::from_str(request_json).map_err(|e| format!("bad request: {e}"))?;
    codec::encode(&ClientFrame::Request { id, body }).map_err(|e| e.to_string())
}

pub fn decode_server_frame_impl(bytes: &[u8]) -> Result<String, String> {
    let frame: ServerFrame = codec::decode(bytes).map_err(|e| format!("not a server frame: {e}"))?;
    serde_json::to_string(&frame).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_answered_with_the_guests_own_signature() {
        let kp = Keypair::generate();
        let nonce = [9u8; 32];
        let challenge = codec::encode(&ServerFrame::Challenge { nonce }).unwrap();
        let bytes = answer_challenge(&kp, &challenge, "ABCD1234").unwrap();
        let ClientFrame::Authenticate { public_key, signed_challenge, invite_code, setup_token } = codec::decode(&bytes).unwrap() else { panic!("not Authenticate") };
        assert_eq!(public_key, kp.public_key());
        public_key.verify(&nonce, &signed_challenge).expect("signature is valid");
        assert_eq!(invite_code.as_deref(), Some("ABCD1234"));
        assert!(setup_token.is_none());
        // A returning guest sends no invite.
        let b2 = answer_challenge(&kp, &challenge, "").unwrap();
        assert!(matches!(codec::decode::<ClientFrame>(&b2).unwrap(), ClientFrame::Authenticate { invite_code: None, .. }));
    }

    #[test]
    fn the_two_join_events_form_one_signed_chain() {
        let kp = Keypair::generate();
        let (first, b1) = begin_join(&kp, 1, "srv", 1000).unwrap();
        let b2 = finish_join(&kp, 2, "srv", "invitehash", &first, 1001).unwrap();
        let ClientFrame::Request { body: ServerRequest::SubmitEvent { event: e1 }, .. } = codec::decode(&b1).unwrap() else { panic!() };
        let ClientFrame::Request { id, body: ServerRequest::SubmitEvent { event: e2 } } = codec::decode(&b2).unwrap() else { panic!() };
        assert_eq!((e1.core.seq, e2.core.seq, id), (0, 1, 2));
        assert_eq!(e2.core.prev, Some(e1.hash()));
        assert!(e2.core.lamport > e1.core.lamport);
        assert_eq!(e1.core.author, kp.public_key());
        let EventPayload::DeviceAuthorized { cert } = &e1.core.payload else { panic!() };
        assert_ne!(cert.core.device_pubkey, kp.public_key(), "the device key is not the identity key");
        // The same secret always gives the same device (a refresh keeps the guest's chain valid).
        assert_eq!(device_key(&kp).public_key(), device_key(&Keypair::from_signing_key_bytes(kp.signing_key_bytes())).public_key());
        let EventPayload::MemberJoined { member, invite } = &e2.core.payload else { panic!() };
        assert_eq!((member, invite.as_str()), (&kp.public_key(), "invitehash"));
    }

    #[test]
    fn anything_but_a_challenge_is_refused() {
        let kp = Keypair::generate();
        let not = codec::encode(&ServerFrame::AuthError { reason: "x".into() }).unwrap();
        assert!(answer_challenge(&kp, &not, "").is_err());
        assert!(answer_challenge(&kp, &[1, 2, 3], "").is_err());
    }

    #[test]
    fn requests_written_as_json_become_the_real_frames() {
        let bytes = encode_request_impl(7, r#"{"SendMessage":{"channel_id":3,"content":"hi","reply_to":null,"attachment_ids":[]}}"#).unwrap();
        let ClientFrame::Request { id, body: ServerRequest::SendMessage { channel_id, content, .. } } = codec::decode(&bytes).unwrap() else { panic!() };
        assert_eq!((id, channel_id, content.as_str()), (7, 3, "hi"));
        // A unit request is a bare string in JSON and is byte-identical to what the app sends.
        let unit = encode_request_impl(1, r#""GetServerInfo""#).unwrap();
        assert_eq!(unit, codec::encode(&ClientFrame::Request { id: 1, body: ServerRequest::GetServerInfo }).unwrap());
        assert!(encode_request_impl(1, r#"{"NoSuchRequest":{}}"#).is_err());
    }

    #[test]
    fn server_frames_become_readable_json() {
        let f = ServerFrame::Authenticated { session_token: vec![1, 2] };
        let json = decode_server_frame_impl(&codec::encode(&f).unwrap()).unwrap();
        assert!(json.contains("Authenticated") && json.contains("session_token"), "{json}");
        assert!(decode_server_frame_impl(&[0xff]).is_err());
    }
}
