//! QUIC transport for cross-host block transfers.
//!
//! Implements the `cgn-kv` `Frame` codec from
//! [`cgn_kv::transport`](../../cgn-kv/src/transport.rs) over QUIC streams.
//!
//! Wire layout per stream:
//! 1. 4-byte big-endian header length, then a bincode-encoded
//!    [`cgn_kv::transport::Frame`].
//! 2. If `frame.op == Push`, `frame.body_len` bytes of payload follow.
//! 3. The receiver replies with an `Ack` frame on the same stream.
//!
//! The same module exposes both the server (`serve_quic`) and a small
//! client (`peer_push`, `peer_pull`) that the gRPC `Push`/`Pull` handlers
//! drive.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use cgn_core::{Error, Result};
use cgn_kv::{
    block::BlockAddress,
    tier::Tier,
    transport::{Frame, Op, ALPN},
};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig};
use tracing::{debug, info, warn};

use crate::tiers::Store;

/// Maximum block size we'll accept on the wire (16 MiB). Larger
/// transfers are rejected to keep memory bounded under hostile peers.
const MAX_BLOCK_BYTES: u64 = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub async fn serve_quic(store: Arc<Store>, addr: SocketAddr) -> Result<()> {
    let server_config = build_server_config(addr)?;
    let endpoint = Endpoint::server(server_config, addr)
        .map_err(|e| Error::Internal(format!("quic listen: {e}")))?;

    info!(%addr, "kvcached quic listening");
    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    debug!(remote = %conn.remote_address(), "quic connection accepted");
                    if let Err(e) = handle_connection(store, conn).await {
                        warn!(error=?e, "quic connection error");
                    }
                }
                Err(e) => warn!(error=?e, "quic accept failed"),
            }
        });
    }
    Ok(())
}

async fn handle_connection(store: Arc<Store>, conn: Connection) -> Result<()> {
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(quinn::ConnectionError::ApplicationClosed(_))
            | Err(quinn::ConnectionError::ConnectionClosed(_))
            | Err(quinn::ConnectionError::Reset) => return Ok(()),
            Err(e) => return Err(Error::Internal(format!("accept_bi: {e}"))),
        };

        let frame = read_frame(&mut recv).await?;
        match frame.op {
            Op::Pull => {
                let bytes = match store.ram.get(&frame.addr) {
                    Some(handle) if handle.meta.bytes > 0 => match read_ram(&store, &frame.addr) {
                        Some(b) => b,
                        None => Bytes::new(),
                    },
                    _ => Bytes::new(),
                };
                let resp = Frame {
                    op: Op::Push,
                    addr: frame.addr,
                    body_len: bytes.len() as u64,
                };
                write_frame(&mut send, &resp).await?;
                send.write_all(&bytes)
                    .await
                    .map_err(|e| Error::Internal(format!("quic write body: {e}")))?;
                send.finish()
                    .map_err(|e| Error::Internal(format!("quic finish: {e}")))?;
            }
            Op::Push => {
                if frame.body_len > MAX_BLOCK_BYTES {
                    return Err(Error::InvalidArgument(format!(
                        "push body {} exceeds max {}",
                        frame.body_len, MAX_BLOCK_BYTES
                    )));
                }
                let mut buf = BytesMut::with_capacity(frame.body_len as usize);
                buf.resize(frame.body_len as usize, 0);
                recv.read_exact(&mut buf)
                    .await
                    .map_err(|e| Error::Internal(format!("quic read body: {e}")))?;
                let bytes = buf.freeze();
                store.put_ram(frame.addr, bytes, "")?;
                let ack = Frame {
                    op: Op::Ack,
                    addr: frame.addr,
                    body_len: 0,
                };
                write_frame(&mut send, &ack).await?;
                send.finish()
                    .map_err(|e| Error::Internal(format!("quic finish: {e}")))?;
            }
            Op::Ack => {
                // Nothing to do; some peers send a no-op ping.
            }
        }
    }
}

fn read_ram(store: &Store, addr: &BlockAddress) -> Option<Bytes> {
    use cgn_kv::tier::Tier;
    let _ = store.ram.get(addr)?;
    // RamTier exposes get-handle but not raw bytes; we re-read directly
    // from the inner DashMap via a tiny helper below.
    store.ram.get_bytes(addr)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Open a one-shot connection to a peer and request the named block.
/// Returns the block bytes (possibly empty if the peer replied with a miss).
pub async fn peer_pull(remote: SocketAddr, addr: BlockAddress) -> Result<Bytes> {
    let endpoint = build_client_endpoint()?;
    let conn = connect(&endpoint, remote).await?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::Internal(format!("open_bi: {e}")))?;
    let frame = Frame {
        op: Op::Pull,
        addr,
        body_len: 0,
    };
    write_frame(&mut send, &frame).await?;
    send.finish()
        .map_err(|e| Error::Internal(format!("finish: {e}")))?;

    let resp = read_frame(&mut recv).await?;
    if resp.body_len > MAX_BLOCK_BYTES {
        return Err(Error::InvalidArgument(format!(
            "peer pulled too much: {} bytes",
            resp.body_len
        )));
    }
    let mut buf = BytesMut::with_capacity(resp.body_len as usize);
    buf.resize(resp.body_len as usize, 0);
    if resp.body_len > 0 {
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| Error::Internal(format!("read body: {e}")))?;
    }
    Ok(buf.freeze())
}

/// Push `bytes` into the named block on the peer.
pub async fn peer_push(remote: SocketAddr, addr: BlockAddress, bytes: Bytes) -> Result<()> {
    let endpoint = build_client_endpoint()?;
    let conn = connect(&endpoint, remote).await?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::Internal(format!("open_bi: {e}")))?;
    let frame = Frame {
        op: Op::Push,
        addr,
        body_len: bytes.len() as u64,
    };
    write_frame(&mut send, &frame).await?;
    send.write_all(&bytes)
        .await
        .map_err(|e| Error::Internal(format!("write body: {e}")))?;
    send.finish()
        .map_err(|e| Error::Internal(format!("finish: {e}")))?;
    let _ack = read_frame(&mut recv).await?;
    Ok(())
}

async fn connect(endpoint: &Endpoint, remote: SocketAddr) -> Result<Connection> {
    let conn = endpoint
        .connect(remote, "cognitora")
        .map_err(|e| Error::Internal(format!("quic connect: {e}")))?
        .await
        .map_err(|e| Error::Unavailable(format!("quic connect await: {e}")))?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Frame codec
// ---------------------------------------------------------------------------

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<Frame> {
    let mut hdr = [0u8; 4];
    recv.read_exact(&mut hdr)
        .await
        .map_err(|e| Error::Internal(format!("read hdr: {e}")))?;
    let len = u32::from_be_bytes(hdr) as usize;
    if len > 16 * 1024 {
        return Err(Error::InvalidArgument(format!(
            "frame header too big: {len}"
        )));
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| Error::Internal(format!("read frame: {e}")))?;
    bincode::deserialize::<Frame>(&payload)
        .map_err(|e| Error::InvalidArgument(format!("decode frame: {e}")))
}

async fn write_frame(send: &mut quinn::SendStream, frame: &Frame) -> Result<()> {
    let bytes =
        bincode::serialize(frame).map_err(|e| Error::Internal(format!("encode frame: {e}")))?;
    let len = (bytes.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .map_err(|e| Error::Internal(format!("write hdr: {e}")))?;
    send.write_all(&bytes)
        .await
        .map_err(|e| Error::Internal(format!("write frame: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// TLS / endpoint setup
// ---------------------------------------------------------------------------

fn build_server_config(addr: SocketAddr) -> Result<ServerConfig> {
    let pki = cgn_tls::generate_dev_pki(
        "cgn-kvcached",
        vec![
            "localhost".into(),
            addr.ip().to_string(),
            "cognitora".into(),
        ],
    )?;

    let cert_chain = vec![rustls_pki_types::CertificateDer::from(pem_decode(
        &pki.leaf_cert_pem,
    )?)];
    let key = rustls_pki_types::PrivateKeyDer::try_from(pem_decode(&pki.leaf_key_pem)?)
        .map_err(|e| Error::Tls(format!("private key: {e}")))?;

    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| Error::Tls(format!("rustls server: {e}")))?;
    rustls_cfg.alpn_protocols = vec![ALPN.to_vec()];

    let mut server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)
            .map_err(|e| Error::Tls(format!("quic server tls: {e}")))?,
    ));

    let transport = Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| Error::Internal("quic transport mutate".into()))?;
    transport.max_concurrent_uni_streams(0u32.into());
    transport.max_idle_timeout(Some(
        Duration::from_secs(30)
            .try_into()
            .map_err(|e| Error::Internal(format!("idle timeout: {e}")))?,
    ));

    Ok(server_config)
}

fn build_client_endpoint() -> Result<Endpoint> {
    // Dev mode: trust any cert (matches the dev PKI on the server side).
    // Production deployments wire this through the cluster CA via
    // `cgn-tls`; see `docs/architecture/security.md`.
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| Error::Tls(format!("quic client tls: {e}")))?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_secs(10)
            .try_into()
            .map_err(|e| Error::Internal(format!("idle timeout: {e}")))?,
    ));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
        .map_err(|e| Error::Internal(format!("quic client bind: {e}")))?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

#[derive(Debug)]
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

fn pem_decode(s: &str) -> Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(s.as_bytes());
    let pemfile = rustls_pemfile::read_all(&mut cursor)
        .next()
        .transpose()
        .map_err(|e| Error::Tls(format!("pem: {e}")))?
        .ok_or_else(|| Error::Tls("empty pem".into()))?;
    Ok(match pemfile {
        rustls_pemfile::Item::X509Certificate(d) => d.to_vec(),
        rustls_pemfile::Item::Pkcs8Key(d) => d.secret_pkcs8_der().to_vec(),
        rustls_pemfile::Item::Pkcs1Key(d) => d.secret_pkcs1_der().to_vec(),
        rustls_pemfile::Item::Sec1Key(d) => d.secret_sec1_der().to_vec(),
        _ => return Err(Error::Tls("unsupported pem item".into())),
    })
}
