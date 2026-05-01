//! `cgn-ctl key …` — API-key management.

use std::path::PathBuf;

use cgn_core::Result;
use clap::Subcommand;
use sha2::{Digest, Sha256};
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Generate a new API key, print it once, append the SHA-256 + scopes
    /// to the keys file.
    Create {
        /// Comma-separated scopes (e.g. "chat,embed").
        #[arg(long, default_value = "chat,embed")]
        scopes: String,
        /// Path to the keys file. Created if absent.
        #[arg(short, long, default_value = "/etc/cognitora/api-keys")]
        file: PathBuf,
    },
    /// Revoke an existing key (matches by SHA-256 prefix or full sha).
    Revoke {
        /// 16-char prefix or full 64-char sha256 of the key.
        sha: String,
        #[arg(short, long, default_value = "/etc/cognitora/api-keys")]
        file: PathBuf,
    },
    /// Rehash plaintext keys in the file in-place.
    Lock {
        #[arg(short, long, default_value = "/etc/cognitora/api-keys")]
        file: PathBuf,
    },
}

pub async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Create { scopes, file } => create(scopes, file),
        Cmd::Revoke { sha, file }    => revoke(&sha, file),
        Cmd::Lock { file }           => lock(file),
    }
}

fn create(scopes: String, file: PathBuf) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(cgn_core::Error::Io)?;
    }
    let secret = format!("cgn-{}", uuid::Uuid::new_v4().simple());
    let sha = sha_hex(secret.as_bytes());
    let line = format!("{sha} {scopes}\n");
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true).append(true).open(&file)
        .map_err(cgn_core::Error::Io)?
        .write_all(line.as_bytes())
        .map_err(cgn_core::Error::Io)?;
    println!("{secret}");
    info!(sha = %sha, file = %file.display(), "key created (only printed once)");
    Ok(())
}

fn revoke(sha: &str, file: PathBuf) -> Result<()> {
    let txt = std::fs::read_to_string(&file).map_err(cgn_core::Error::Io)?;
    let needle = sha.to_ascii_lowercase();
    let kept: Vec<&str> = txt.lines().filter(|l| {
        let first = l.split_whitespace().next().unwrap_or("");
        !first.starts_with(&needle)
    }).collect();
    std::fs::write(&file, kept.join("\n")).map_err(cgn_core::Error::Io)?;
    info!(sha = %sha, "revoked");
    Ok(())
}

fn lock(file: PathBuf) -> Result<()> {
    let txt = std::fs::read_to_string(&file).map_err(cgn_core::Error::Io)?;
    let mut out = String::new();
    for line in txt.lines() {
        let mut parts = line.split_whitespace();
        let Some(token) = parts.next() else { out.push('\n'); continue; };
        let scopes = parts.next().unwrap_or("");
        if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push_str(line);
        } else {
            out.push_str(&format!("{} {scopes}", sha_hex(token.as_bytes())));
        }
        out.push('\n');
    }
    std::fs::write(&file, out).map_err(cgn_core::Error::Io)?;
    Ok(())
}

fn sha_hex(input: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(input);
    hex::encode(h.finalize())
}
