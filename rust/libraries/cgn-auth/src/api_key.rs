//! Hashed API-key store with hot-reload from disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use cgn_core::{Error, Result};
use sha2::{Digest, Sha256};

use super::{Principal, PrincipalKind};

#[derive(Default, Clone)]
struct Store(HashMap<[u8; 32], Vec<String>>);

/// In-memory map `sha256(api-key) -> scopes`. Reload-safe.
#[derive(Clone)]
pub struct ApiKeyStore {
    inner: Arc<ArcSwap<Store>>,
    path:  Option<PathBuf>,
}

impl ApiKeyStore {
    pub fn empty() -> Self {
        Self { inner: Arc::new(ArcSwap::from_pointee(Store::default())), path: None }
    }

    /// Load `path`. Format: one record per line.
    /// `<sha256-hex> [scope1,scope2,...]` or `<plaintext-key> [scopes]`
    /// (plaintext is hashed on the fly).
    pub fn from_file(path: &Path) -> Result<Self> {
        let txt = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("api keys file: {e}")))?;
        let store = parse(&txt)?;
        Ok(Self {
            inner: Arc::new(ArcSwap::from_pointee(store)),
            path:  Some(path.to_path_buf()),
        })
    }

    /// Re-read `self.path` from disk (no-op if no file was provided).
    pub fn reload(&self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        let txt = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("api keys file: {e}")))?;
        let store = parse(&txt)?;
        self.inner.store(Arc::new(store));
        Ok(())
    }

    /// Validate a presented bearer token. Returns `Some(Principal)` on hit.
    pub fn check(&self, presented: &str) -> Option<Principal> {
        let digest = sha256(presented.as_bytes());
        let snap = self.inner.load();
        let scopes = snap.0.get(&digest)?;
        Some(Principal {
            subject: format!("apikey:{}", &hex_short(&digest)),
            scopes:  scopes.clone(),
            kind:    PrincipalKind::ApiKey,
        })
    }

    pub fn len(&self) -> usize { self.inner.load().0.len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

fn parse(txt: &str) -> Result<Store> {
    let mut out = HashMap::new();
    for (lineno, raw) in txt.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let mut it = line.split_whitespace();
        let token = it.next().ok_or_else(|| Error::Config(format!("api keys line {}: empty", lineno + 1)))?;
        let scopes = it.next().map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
        let digest = if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut d = [0u8; 32];
            for (i, chunk) in token.as_bytes().chunks(2).enumerate() {
                d[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16)
                    .map_err(|e| Error::Config(format!("api keys line {}: hex: {e}", lineno + 1)))?;
            }
            d
        } else {
            sha256(token.as_bytes())
        };
        out.insert(digest, scopes);
    }
    Ok(Store(out))
}

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(input);
    let r = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&r);
    out
}

fn hex_short(d: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(16);
    for b in &d[..8] {
        write!(&mut s, "{b:02x}").ok();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plaintext_and_hashed() {
        let txt = "\
            # comment
            cgn-secret-1 chat,embed
            0000000000000000000000000000000000000000000000000000000000000000 admin
        ";
        let s = ApiKeyStore { inner: Arc::new(ArcSwap::from_pointee(parse(txt).unwrap())), path: None };
        assert_eq!(s.len(), 2);
        let p = s.check("cgn-secret-1").unwrap();
        assert!(p.has_scope("chat"));
        assert_eq!(p.kind, PrincipalKind::ApiKey);
    }
}
