//! Thin wrapper around the `helm` binary.
//!
//! `cgn-ctl install` orchestrates a Cognitora install by invoking helm under
//! the hood. Rust's ecosystem doesn't have a real Helm SDK, but shelling
//! out is fine: helm is a single static Go binary, we ship it embedded
//! in our release tarballs, and the surface we use is stable.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use cgn_core::{Error, Result};
use serde::Serialize;
use tokio::process::Command;

/// Locate the helm binary. Honours `$CGN_HELM_BIN`, falls back to PATH lookup.
pub fn locate_helm() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CGN_HELM_BIN") {
        return Ok(PathBuf::from(p));
    }
    which::which("helm")
        .map_err(|_| Error::Unavailable("helm binary not found in PATH; set CGN_HELM_BIN".into()))
}

/// `helm version --short` round-trip, used by `cgn-ctl preflight`.
pub async fn version() -> Result<String> {
    let bin = locate_helm()?;
    let out = Command::new(bin)
        .args(["version", "--short"])
        .output()
        .await
        .map_err(|e| Error::Internal(format!("helm version: {e}")))?;
    if !out.status.success() {
        return Err(Error::Unavailable(format!(
            "helm version: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Install or upgrade a chart (`helm upgrade --install`).
#[derive(Debug, Clone)]
pub struct Install {
    pub release:    String,
    pub chart:      PathBuf, // local chart dir or oci:// URL
    pub namespace:  String,
    pub create_namespace: bool,
    pub values:     Vec<PathBuf>,
    pub set:        Vec<(String, String)>,
    pub wait:       bool,
    pub timeout:    Option<String>,
}

impl Install {
    pub async fn run(&self) -> Result<String> {
        let bin = locate_helm()?;
        let mut cmd = Command::new(bin);
        cmd.args([
            "upgrade", "--install",
            &self.release,
            self.chart.to_str().ok_or_else(|| Error::InvalidArgument("chart path utf-8".into()))?,
            "--namespace", &self.namespace,
        ]);
        if self.create_namespace { cmd.arg("--create-namespace"); }
        if self.wait { cmd.arg("--wait"); }
        if let Some(t) = &self.timeout {
            cmd.args(["--timeout", t]);
        }
        for v in &self.values {
            cmd.arg("-f").arg(v);
        }
        for (k, v) in &self.set {
            cmd.arg("--set").arg(format!("{k}={v}"));
        }
        cmd.stdin(Stdio::null());
        run(cmd, "helm upgrade").await
    }
}

/// `helm uninstall <release> -n <ns>`.
pub async fn uninstall(release: &str, namespace: &str) -> Result<String> {
    let bin = locate_helm()?;
    let mut cmd = Command::new(bin);
    cmd.args(["uninstall", release, "--namespace", namespace]);
    run(cmd, "helm uninstall").await
}

/// `helm template ... | yq` equivalent for offline rendering. Returns YAML.
pub async fn template(chart: &Path, values: &[(String, String)]) -> Result<String> {
    let bin = locate_helm()?;
    let mut cmd = Command::new(bin);
    cmd.args(["template", "cognitora", chart.to_str().unwrap_or(".")]);
    for (k, v) in values {
        cmd.arg("--set").arg(format!("{k}={v}"));
    }
    run(cmd, "helm template").await
}

async fn run(mut cmd: Command, what: &str) -> Result<String> {
    let out = cmd.stderr(Stdio::piped()).stdout(Stdio::piped()).output().await
        .map_err(|e| Error::Internal(format!("{what}: spawn: {e}")))?;
    if !out.status.success() {
        return Err(Error::Internal(format!(
            "{what} exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Convenience: render `Vec<u8>` of YAML for a chart from a serialisable
/// `values` struct. Useful in tests and `cgn-ctl install --dry-run`.
pub async fn template_with_values<V: Serialize>(chart: &Path, values: &V) -> Result<String> {
    let yaml = serde_yaml::to_string(values)
        .map_err(|e| Error::Internal(format!("yaml: {e}")))?;
    let tmp = tempfile::NamedTempFile::new()
        .map_err(|e| Error::Io(e))?;
    std::fs::write(tmp.path(), yaml).map_err(Error::Io)?;
    let bin = locate_helm()?;
    let mut cmd = Command::new(bin);
    cmd.args(["template", "cognitora", chart.to_str().unwrap_or(".")])
       .arg("-f").arg(tmp.path());
    run(cmd, "helm template").await
}
