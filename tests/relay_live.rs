//! Does the deployed relay present the certificate the client is pinned to?
//!
//! `DEFAULT_RELAY` in `client/src-tauri/src/default_relay.rs` is a promise about
//! a machine somewhere else: this address answers, and it proves itself with
//! this exact certificate. Nothing in the normal suite can check that, because
//! the answer lives on the internet — so the value gets copied from a deploy
//! log and trusted, and one wrong character produces a client that refuses every
//! connection with no useful error.
//!
//! **`#[ignore]` on purpose.** It needs network and a live relay, so it must
//! never run in the ordinary `cargo test` pass. Run it deliberately, after a
//! relay deploy and again whenever you doubt the constant:
//!
//! ```text
//! cargo test --test relay_live -- --ignored --nocapture
//! ```
//!
//! Update the two constants below when the relay moves. They are deliberately
//! duplicated from the client rather than imported: the client is a binary
//! crate this harness cannot depend on, and a copy that must be kept in step is
//! exactly what this test then verifies against reality.

use std::net::SocketAddr;
use std::sync::Arc;

/// Must match `DEFAULT_RELAY.addr` in the client.
const RELAY_ADDR: &str = "45.61.136.29:4433";
/// Must match `DEFAULT_RELAY.cert_fp_hex` in the client.
const RELAY_FP_HEX: &str = "cbe837a3b1858c7f2246d290761101aa3e53157ca52c3cfb6128a010bd1d9bf0";

/// Captures the certificate the peer actually presented, and accepts it
/// unconditionally — the assertion is the test's job, not the verifier's.
///
/// The real client pins instead (`make_pinned_relay_endpoint`), which fails the
/// handshake on a mismatch. That failure mode is useless for diagnosis: it says
/// "connection refused", not "the fingerprint is stale and here is the real
/// one". This captures so the test can print both.
#[derive(Debug)]
struct CaptureCert(Arc<std::sync::Mutex<Option<Vec<u8>>>>);

impl rustls::client::danger::ServerCertVerifier for CaptureCert {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(end_entity.as_ref().to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[tokio::test]
#[ignore = "needs the network and a deployed relay; run with --ignored after a deploy"]
async fn the_deployed_relay_presents_the_pinned_certificate() {
    use sha2::{Digest, Sha256};

    let _ = rustls::crypto::ring::default_provider().install_default();

    let addr: SocketAddr = RELAY_ADDR.parse().expect("RELAY_ADDR must be IP:port");
    let seen = Arc::new(std::sync::Mutex::new(None));

    // No ALPN, deliberately: neither `make_pinned_relay_endpoint` nor the
    // relay's `create_endpoint` sets any, and offering one here made the relay
    // close the handshake with "peer doesn't support any known protocol". This
    // test is only meaningful if it connects the way the client connects.
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(CaptureCert(seen.clone())))
        .with_no_client_auth();

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
        .expect("bind a local UDP socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic tls config"),
    )));

    let connecting = endpoint
        .connect(addr, "farder-relay")
        .expect("start the handshake");
    let conn = tokio::time::timeout(std::time::Duration::from_secs(12), connecting)
        .await
        .expect("the relay did not answer within 12s — is UDP 4433 open?")
        .expect("QUIC handshake failed");

    let der = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the peer presented no certificate");
    let actual = format!("{:x}", Sha256::digest(&der));

    println!("relay {RELAY_ADDR} presented {actual}");
    assert_eq!(
        actual, RELAY_FP_HEX,
        "\nThe relay's certificate does NOT match the pinned fingerprint.\n\
         pinned: {RELAY_FP_HEX}\n\
         actual: {actual}\n\
         Either the client constant is stale, or that address is no longer your relay.\n"
    );

    conn.close(0u32.into(), b"fingerprint verified");
    endpoint.wait_idle().await;
}
