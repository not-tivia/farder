use anyhow::Result;
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

/// Load the relay's TLS cert+key from `<data_dir>/relay_cert.der` and
/// `relay_key.der`, generating and persisting a self-signed pair on first run.
/// Persisting it gives the relay a stable identity a client can later pin.
fn load_or_generate_cert(
    data_dir: &Path,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    std::fs::create_dir_all(data_dir)?;
    let cert_path = data_dir.join("relay_cert.der");
    let key_path = data_dir.join("relay_key.der");

    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read(&cert_path)?;
        let key = std::fs::read(&key_path)?;
        let key = PrivateKeyDer::try_from(key).map_err(|e| anyhow::anyhow!("key parse: {}", e))?;
        return Ok((CertificateDer::from(cert), key));
    }

    let certified = rcgen::generate_simple_self_signed(vec!["farder-relay".to_string()])?;
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();
    std::fs::write(&cert_path, &cert_der)?;
    // NOTE: first-run is check-then-write; two relays booting on the same
    // data_dir simultaneously could race to generate mismatched cert/key.
    // Low-risk for a single relay process; revisit with atomic write/lock if
    // we ever run multiple relays sharing a data dir.
    std::fs::write(&key_path, &key_der)?;
    // The private key must not be world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    let key = PrivateKeyDer::try_from(key_der).map_err(|e| anyhow::anyhow!("key parse: {}", e))?;
    Ok((CertificateDer::from(cert_der), key))
}

pub fn create_endpoint(bind_addr: SocketAddr, data_dir: &Path) -> Result<Endpoint> {
    let (cert_der, key_der) = load_or_generate_cert(data_dir)?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(60)
            .try_into()
            .map_err(|e| anyhow::anyhow!("idle timeout: {:?}", e))?,
    ));
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
    server_config.transport_config(Arc::new(transport));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    info!("Relay listening on {}", bind_addr);
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_persists_across_calls_and_differs_per_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (c1, _k1) = load_or_generate_cert(dir.path()).unwrap();
        let (c2, _k2) = load_or_generate_cert(dir.path()).unwrap();
        assert_eq!(c1.as_ref(), c2.as_ref(), "cert must persist across calls in the same dir");

        let dir2 = tempfile::tempdir().unwrap();
        let (c3, _k3) = load_or_generate_cert(dir2.path()).unwrap();
        assert_ne!(c1.as_ref(), c3.as_ref(), "a fresh dir must get its own cert");
    }
}
