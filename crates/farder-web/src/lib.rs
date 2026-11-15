//! Farder's real protocol and signing, for the browser guest page (docs/web-guest-join-design.md,
//! step 3). The same Rust that the app and server use, compiled to WebAssembly, so the page cannot
//! drift from the wire format and the guest's key is made and kept in the browser.
//!
//! The page deals in two things only: JSON text (readable requests and answers) and bytes (what goes
//! over the WebSocket, one message per frame). Everything between is here.
//!
//! The logic is plain Rust functions (tested natively); the `#[wasm_bindgen]` layer is a thin wrapper.

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
}

#[wasm_bindgen]
impl GuestKey {
    /// A new random key (from the browser's secure random source).
    #[wasm_bindgen(constructor)]
    pub fn new() -> GuestKey {
        GuestKey { kp: Keypair::generate() }
    }

    /// Rebuild a key the page saved earlier (so a refresh does not log the guest out).
    pub fn from_secret(secret: &[u8]) -> Result<GuestKey, JsError> {
        let arr: [u8; 32] = secret.try_into().map_err(|_| JsError::new("a key is 32 bytes"))?;
        Ok(GuestKey { kp: Keypair::from_signing_key_bytes(&arr) })
    }

    /// The secret, for the page to keep in the browser's own storage. Never send it anywhere.
    pub fn secret(&self) -> Vec<u8> {
        self.kp.signing_key_bytes().to_vec()
    }

    /// The public key as the app shows it (`vk_` and 64 hex characters).
    pub fn public_key(&self) -> String {
        self.kp.public_key().to_string()
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
