//! TLS / mTLS helpers for inter-service gRPC.
//!
//! Cognitora speaks gRPC across hosts and uses mutual TLS for everything
//! that crosses a network boundary. This crate exposes a couple of small
//! helpers built on top of `rustls` and `tonic`:
//!
//! * [`load_identity`]  – read a PEM cert + key into a tonic `Identity`.
//! * [`server_tls`]     – assemble a tonic `ServerTlsConfig` requiring mTLS.
//! * [`client_tls`]     – assemble a tonic `ClientTlsConfig` against a CA.
//! * [`generate_dev_pki`] – bootstrap a self-signed CA + leaf for `cgn-ctl pki`.

#![forbid(unsafe_code)]

use std::path::Path;

use cgn_core::{Error, Result};
use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

/// Load a PEM-encoded certificate + private key from disk.
pub fn load_identity(cert_path: &Path, key_path: &Path) -> Result<Identity> {
    let cert = std::fs::read(cert_path)
        .map_err(|e| Error::Tls(format!("read {}: {e}", cert_path.display())))?;
    let key = std::fs::read(key_path)
        .map_err(|e| Error::Tls(format!("read {}: {e}", key_path.display())))?;
    Ok(Identity::from_pem(cert, key))
}

/// Server-side TLS config requiring client certs (mTLS).
///
/// `ca_path` is the trust root used to verify peer certs; `cert_path` /
/// `key_path` are the server's own identity.
pub fn server_tls(ca_path: &Path, cert_path: &Path, key_path: &Path) -> Result<ServerTlsConfig> {
    let identity = load_identity(cert_path, key_path)?;
    let ca = std::fs::read(ca_path)
        .map_err(|e| Error::Tls(format!("read {}: {e}", ca_path.display())))?;
    Ok(ServerTlsConfig::new()
        .identity(identity)
        .client_ca_root(Certificate::from_pem(ca)))
}

/// Client-side TLS config that trusts `ca_path` and presents `cert/key`.
pub fn client_tls(
    ca_path: &Path,
    cert_path: &Path,
    key_path: &Path,
    domain: impl Into<String>,
) -> Result<ClientTlsConfig> {
    let identity = load_identity(cert_path, key_path)?;
    let ca = std::fs::read(ca_path)
        .map_err(|e| Error::Tls(format!("read {}: {e}", ca_path.display())))?;
    Ok(ClientTlsConfig::new()
        .domain_name(domain)
        .ca_certificate(Certificate::from_pem(ca))
        .identity(identity))
}

/// Generate a self-signed CA + a leaf cert valid for `subject_alt_names`,
/// returning PEM bytes for `(ca_cert, ca_key, leaf_cert, leaf_key)`.
///
/// This is a developer convenience used by `cgn-ctl pki bootstrap`. Use a
/// real PKI (Vault / cert-manager / step-ca) in production.
pub fn generate_dev_pki(common_name: &str, subject_alt_names: Vec<String>) -> Result<DevPki> {
    use rcgen::{CertificateParams, IsCa, KeyUsagePurpose};

    let mut ca_params = CertificateParams::new(vec!["cognitora-dev-ca".into()])
        .map_err(|e| Error::Tls(format!("ca params: {e}")))?;
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Cognitora Dev CA");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = rcgen::KeyPair::generate()
        .map_err(|e| Error::Tls(format!("ca keypair: {e}")))?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| Error::Tls(format!("ca self-sign: {e}")))?;

    let mut leaf_params = CertificateParams::new(subject_alt_names)
        .map_err(|e| Error::Tls(format!("leaf params: {e}")))?;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name);
    let leaf_key = rcgen::KeyPair::generate()
        .map_err(|e| Error::Tls(format!("leaf keypair: {e}")))?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .map_err(|e| Error::Tls(format!("leaf sign: {e}")))?;

    Ok(DevPki {
        ca_cert_pem: ca_cert.pem(),
        ca_key_pem:  ca_key.serialize_pem(),
        leaf_cert_pem: leaf_cert.pem(),
        leaf_key_pem:  leaf_key.serialize_pem(),
    })
}

/// Output of [`generate_dev_pki`].
#[derive(Debug, Clone)]
pub struct DevPki {
    pub ca_cert_pem: String,
    pub ca_key_pem: String,
    pub leaf_cert_pem: String,
    pub leaf_key_pem: String,
}
