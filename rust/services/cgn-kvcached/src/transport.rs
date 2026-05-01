//! QUIC transport for cross-host block transfers.
//!
//! Currently a skeleton: accepts inbound connections, logs the ALPN, and
//! returns. The full read/write protocol (see `cgn_kv::transport::Frame`) is
//! tracked in M3 of the rollout plan.

use std::net::SocketAddr;
use std::sync::Arc;

use cgn_core::{Error, Result};
use tracing::{info, warn};

use crate::tiers::Store;

pub async fn serve_quic(_store: Arc<Store>, addr: SocketAddr) -> Result<()> {
    // Self-signed identity for dev: full deployments use mTLS material from
    // `[security]` in cognitora.toml; this binding is short-lived and
    // unauthenticated traffic is rejected by the kv router-side handshake.
    let pki = cgn_tls::generate_dev_pki(
        "cgn-kvcached",
        vec!["localhost".into(), addr.ip().to_string()],
    )?;

    let cert_chain = vec![rustls_pki_types::CertificateDer::from(
        pem_decode(&pki.leaf_cert_pem)?,
    )];
    let key = rustls_pki_types::PrivateKeyDer::try_from(pem_decode(&pki.leaf_key_pem)?)
        .map_err(|e| Error::Tls(format!("private key: {e}")))?;

    let mut server_config = quinn::ServerConfig::with_single_cert(cert_chain, key)
        .map_err(|e| Error::Tls(format!("quic server config: {e}")))?;
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .max_concurrent_uni_streams(0u32.into());

    let endpoint = quinn::Endpoint::server(server_config, addr)
        .map_err(|e| Error::Internal(format!("quic listen: {e}")))?;

    info!(%addr, "kvcached quic listening (skeleton)");
    while let Some(conn) = endpoint.accept().await {
        tokio::spawn(async move {
            match conn.await {
                Ok(c) => info!(remote = %c.remote_address(), "quic connection"),
                Err(e) => warn!(error=?e, "quic accept"),
            }
        });
    }
    Ok(())
}

fn pem_decode(s: &str) -> Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(s.as_bytes());
    let pemfile = rustls_pemfile::read_all(&mut cursor)
        .next()
        .transpose()
        .map_err(|e| Error::Tls(format!("pem: {e}")))?
        .ok_or_else(|| Error::Tls("empty pem".into()))?;
    Ok(match pemfile {
        rustls_pemfile::Item::X509Certificate(d)  => d.to_vec(),
        rustls_pemfile::Item::Pkcs8Key(d)         => d.secret_pkcs8_der().to_vec(),
        rustls_pemfile::Item::Pkcs1Key(d)         => d.secret_pkcs1_der().to_vec(),
        rustls_pemfile::Item::Sec1Key(d)          => d.secret_sec1_der().to_vec(),
        _ => return Err(Error::Tls("unsupported pem item".into())),
    })
}
