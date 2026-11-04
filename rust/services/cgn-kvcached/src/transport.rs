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
//!
//! # Transport security
//!
//! Peer traffic carries KV blocks derived from user prompts, so it is
//! authenticated whenever it can leave the host:
//!
//! * `[security].require_mtls = true`: both sides present certificates
//!   chained to the cluster CA (`[security].ca_file`). Peers without one
//!   fail the handshake.
//! * Otherwise the listener must be loopback, or the operator must opt in
//!   with `[kv].allow_insecure_quic = true`. Startup fails instead of
//!   silently exposing an unauthenticated endpoint.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use cgn_core::{Error, Result};
use cgn_kv::{
    block::BlockAddress,
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

/// How this process authenticates QUIC peers.
#[derive(Debug, Clone)]
pub enum QuicSecurity {
    /// Mutual TLS against the cluster CA.
    Mtls {
        ca: std::path::PathBuf,
        cert: std::path::PathBuf,
        key: std::path::PathBuf,
        /// Name peers must present in their certificate SANs.
        server_name: String,
    },
    /// Encrypted but unauthenticated. Only reachable through
    /// [`QuicSecurity::from_config`] on loopback or by explicit opt-in.
    Insecure,
}

impl QuicSecurity {
    /// Resolve the policy for `listen`. Fails closed: a non-loopback
    /// listener without mTLS needs `allow_insecure_quic = true`.
    pub fn from_config(cfg: &cgn_core::config::Config, listen: SocketAddr) -> Result<Self> {
        if cfg.security.require_mtls {
            let (Some(ca), Some(cert), Some(key)) = (
                cfg.security.ca_file.clone(),
                cfg.security.cert_file.clone(),
                cfg.security.key_file.clone(),
            ) else {
                return Err(Error::Config(
                    "require_mtls=true but [security].ca_file/cert_file/key_file are not set"
                        .into(),
                ));
            };
            return Ok(Self::Mtls {
                ca,
                cert,
                key,
                server_name: cfg.kv.quic_server_name.clone(),
            });
        }
        if listen.ip().is_loopback() {
            return Ok(Self::Insecure);
        }
        if cfg.kv.allow_insecure_quic {
            warn!(%listen, "kv quic is UNAUTHENTICATED: allow_insecure_quic=true on a non-loopback listener");
            return Ok(Self::Insecure);
        }
        Err(Error::Config(format!(
            "kv.quic_listen {listen} is not loopback and [security].require_mtls is false; \
             enable mTLS, bind to 127.0.0.1, or set [kv].allow_insecure_quic = true"
        )))
    }

    fn server_name(&self) -> &str {
        match self {
            Self::Mtls { server_name, .. } => server_name,
            Self::Insecure => "cognitora",
        }
    }
}

static SECURITY: OnceLock<QuicSecurity> = OnceLock::new();

/// Install the process-wide policy used by [`peer_pull`] / [`peer_push`].
pub fn install_security(sec: QuicSecurity) {
    let _ = SECURITY.set(sec);
}

fn security() -> Result<&'static QuicSecurity> {
    SECURITY
        .get()
        .ok_or_else(|| Error::Internal("quic security policy not installed".into()))
}

pub async fn serve_quic(store: Arc<Store>, addr: SocketAddr, sec: QuicSecurity) -> Result<()> {
    let server_config = build_server_config(addr, &sec)?;
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
                let bytes = store.ram.get_bytes(&frame.addr).unwrap_or_default();
                let resp = Frame {
                    op: Op::Push,
                    addr: frame.addr,
                    body_len: bytes.len() as u64,
                };
                write_frame(&mut send, &resp).await?;
                store
                    .stats
                    .bytes_pushed
                    .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
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
                store
                    .stats
                    .bytes_pulled
                    .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
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

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Open a one-shot connection to a peer and request the named block.
/// Returns the block bytes (possibly empty if the peer replied with a miss).
pub async fn peer_pull(remote: SocketAddr, addr: BlockAddress) -> Result<Bytes> {
    peer_pull_with(security()?, remote, addr).await
}

async fn peer_pull_with(
    sec: &QuicSecurity,
    remote: SocketAddr,
    addr: BlockAddress,
) -> Result<Bytes> {
    let endpoint = build_client_endpoint(sec)?;
    let conn = connect(&endpoint, remote, sec.server_name()).await?;
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
    let sec = security()?;
    let endpoint = build_client_endpoint(sec)?;
    let conn = connect(&endpoint, remote, sec.server_name()).await?;
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

async fn connect(endpoint: &Endpoint, remote: SocketAddr, name: &str) -> Result<Connection> {
    let conn = endpoint
        .connect(remote, name)
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

fn read_certs(path: &std::path::Path) -> Result<Vec<rustls_pki_types::CertificateDer<'static>>> {
    let pem =
        std::fs::read(path).map_err(|e| Error::Tls(format!("read {}: {e}", path.display())))?;
    rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Tls(format!("parse {}: {e}", path.display())))
}

fn read_key(path: &std::path::Path) -> Result<rustls_pki_types::PrivateKeyDer<'static>> {
    let pem =
        std::fs::read(path).map_err(|e| Error::Tls(format!("read {}: {e}", path.display())))?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        .map_err(|e| Error::Tls(format!("parse {}: {e}", path.display())))?
        .ok_or_else(|| Error::Tls(format!("no private key in {}", path.display())))
}

fn root_store(ca: &std::path::Path) -> Result<Arc<rustls::RootCertStore>> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in read_certs(ca)? {
        roots
            .add(cert)
            .map_err(|e| Error::Tls(format!("ca {}: {e}", ca.display())))?;
    }
    Ok(Arc::new(roots))
}

fn build_server_config(addr: SocketAddr, sec: &QuicSecurity) -> Result<ServerConfig> {
    let mut rustls_cfg = match sec {
        QuicSecurity::Mtls { ca, cert, key, .. } => {
            let verifier = rustls::server::WebPkiClientVerifier::builder(root_store(ca)?)
                .build()
                .map_err(|e| Error::Tls(format!("client verifier: {e}")))?;
            rustls::ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(read_certs(cert)?, read_key(key)?)
                .map_err(|e| Error::Tls(format!("rustls server: {e}")))?
        }
        QuicSecurity::Insecure => {
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
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert_chain, key)
                .map_err(|e| Error::Tls(format!("rustls server: {e}")))?
        }
    };
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

fn build_client_endpoint(sec: &QuicSecurity) -> Result<Endpoint> {
    let mut crypto = match sec {
        QuicSecurity::Mtls { ca, cert, key, .. } => rustls::ClientConfig::builder()
            .with_root_certificates(root_store(ca)?)
            .with_client_auth_cert(read_certs(cert)?, read_key(key)?)
            .map_err(|e| Error::Tls(format!("rustls client: {e}")))?,
        QuicSecurity::Insecure => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::config::{KvConfig, SecurityConfig};
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};

    struct Pki {
        _dir: tempfile::TempDir,
        ca: std::path::PathBuf,
        cert: std::path::PathBuf,
        key: std::path::PathBuf,
    }

    fn pki() -> Pki {
        let dir = tempfile::tempdir().unwrap();
        let p = cgn_tls::generate_dev_pki("peer", vec!["cognitora".into()]).unwrap();
        let (ca, cert, key) = (
            dir.path().join("ca.pem"),
            dir.path().join("leaf.pem"),
            dir.path().join("leaf.key"),
        );
        std::fs::write(&ca, p.ca_cert_pem).unwrap();
        std::fs::write(&cert, p.leaf_cert_pem).unwrap();
        std::fs::write(&key, p.leaf_key_pem).unwrap();
        Pki {
            _dir: dir,
            ca,
            cert,
            key,
        }
    }

    fn mtls(ca: &Pki, id: &Pki) -> QuicSecurity {
        QuicSecurity::Mtls {
            ca: ca.ca.clone(),
            cert: id.cert.clone(),
            key: id.key.clone(),
            server_name: "cognitora".into(),
        }
    }

    fn free_addr() -> SocketAddr {
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn addr() -> BlockAddress {
        BlockAddress {
            digest: [7u8; 32],
            layer: 0,
        }
    }

    async fn server(sec: QuicSecurity) -> (SocketAddr, Arc<Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = KvConfig {
            ram_gib: 1,
            ssd_gib: 1,
            ssd_dir: dir.path().join("ssd"),
            index_dir: dir.path().join("idx"),
            ..KvConfig::default()
        };
        let store = Arc::new(Store::open(&cfg).await.unwrap());
        store
            .put_ram(addr(), Bytes::from_static(b"secret"), "")
            .unwrap();
        let listen = free_addr();
        tokio::spawn(serve_quic(store.clone(), listen, sec));
        tokio::time::sleep(Duration::from_millis(100)).await;
        (listen, store, dir)
    }

    #[tokio::test]
    async fn mtls_peer_with_cluster_cert_reads_block() {
        let p = pki();
        let (listen, _s, _d) = server(mtls(&p, &p)).await;
        let got = peer_pull_with(&mtls(&p, &p), listen, addr()).await.unwrap();
        assert_eq!(&got[..], b"secret");
    }

    #[tokio::test]
    async fn mtls_rejects_client_without_certificate() {
        let p = pki();
        let (listen, _s, _d) = server(mtls(&p, &p)).await;
        // Insecure client presents no certificate. TLS 1.3 client auth
        // fails after the client thinks the handshake finished, so the
        // rejection may surface on the first stream instead of connect.
        let got = tokio::time::timeout(
            Duration::from_secs(5),
            peer_pull_with(&QuicSecurity::Insecure, listen, addr()),
        )
        .await
        .expect("handshake must fail, not hang");
        assert!(got.is_err(), "client without a cert read a block: {got:?}");
    }

    #[tokio::test]
    async fn mtls_rejects_client_from_another_ca() {
        let (cluster, rogue) = (pki(), pki());
        let (listen, _s, _d) = server(mtls(&cluster, &cluster)).await;
        let got = tokio::time::timeout(
            Duration::from_secs(5),
            peer_pull_with(&mtls(&cluster, &rogue), listen, addr()),
        )
        .await
        .expect("handshake must fail, not hang");
        assert!(got.is_err(), "rogue client read a block: {got:?}");
    }

    #[tokio::test]
    async fn mtls_client_rejects_server_from_another_ca() {
        let (cluster, rogue) = (pki(), pki());
        let (listen, _s, _d) = server(mtls(&rogue, &rogue)).await;
        let got = peer_pull_with(&mtls(&cluster, &cluster), listen, addr()).await;
        assert!(got.is_err(), "client trusted a rogue server");
    }

    fn cfg(require_mtls: bool, allow_insecure: bool) -> cgn_core::config::Config {
        cgn_core::config::Config {
            security: SecurityConfig {
                require_mtls,
                ..SecurityConfig::default()
            },
            kv: KvConfig {
                allow_insecure_quic: allow_insecure,
                ..KvConfig::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn policy_fails_closed_on_public_listener_without_mtls() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7091);
        assert!(QuicSecurity::from_config(&cfg(false, false), public).is_err());
        assert!(matches!(
            QuicSecurity::from_config(&cfg(false, true), public),
            Ok(QuicSecurity::Insecure)
        ));
    }

    #[test]
    fn policy_allows_loopback_and_requires_files_for_mtls() {
        let lo = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7091);
        assert!(matches!(
            QuicSecurity::from_config(&cfg(false, false), lo),
            Ok(QuicSecurity::Insecure)
        ));
        assert!(QuicSecurity::from_config(&cfg(true, false), lo).is_err());
    }
}
