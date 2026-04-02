use anyhow::Result;
use quinn::Endpoint;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

pub fn create_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-relay".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
            .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    info!("Relay listening on {}", bind_addr);
    Ok(endpoint)
}
