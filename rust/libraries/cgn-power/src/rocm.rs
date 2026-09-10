//! AMD ROCm power reader.
//!
//! Shells out to `rocm-smi --showpower --json` and sums per-card package
//! power. Field names vary across ROCm releases (`Average Graphics
//! Package Power (W)`, `Current Socket Graphics Package Power (W)`, …),
//! so matching is by tolerant substring rather than exact key. Hosts
//! without `rocm-smi` become a no-op that returns an empty list, same as
//! the NVML reader on non-NVIDIA boxes.

use async_trait::async_trait;
use cgn_core::Result;
use std::sync::OnceLock;

use super::{PowerReader, PowerSample};

pub struct Rocm;

impl Rocm {
    pub fn new() -> Self {
        static INIT: OnceLock<bool> = OnceLock::new();
        INIT.get_or_init(|| {
            let ok = std::process::Command::new("rocm-smi")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                tracing::debug!("rocm-smi not available; AMD GPU power disabled");
            }
            ok
        });
        Self
    }

    fn available() -> bool {
        std::process::Command::new("rocm-smi")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Default for Rocm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PowerReader for Rocm {
    fn name(&self) -> &'static str {
        "rocm"
    }

    async fn sample(&self) -> Result<Vec<PowerSample>> {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        if !*AVAILABLE.get_or_init(Self::available) {
            return Ok(vec![]);
        }
        // rocm-smi is fast (<100 ms) but still a subprocess: run it off
        // the async executor.
        let out = tokio::task::spawn_blocking(|| {
            std::process::Command::new("rocm-smi")
                .args(["--showpower", "--json"])
                .output()
        })
        .await
        .map_err(|e| cgn_core::Error::Internal(format!("rocm-smi join: {e}")))?
        .map_err(|e| cgn_core::Error::Internal(format!("rocm-smi: {e}")))?;

        let total = parse_power_watts(&String::from_utf8_lossy(&out.stdout));
        let now = chrono::Utc::now().timestamp();
        Ok(vec![PowerSample {
            watts: total,
            component: "gpu",
            at_unix: now,
        }])
    }
}

/// Sum per-card power from `rocm-smi --showpower --json` output.
fn parse_power_watts(json: &str) -> f64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return 0.0;
    };
    let Some(obj) = v.as_object() else {
        return 0.0;
    };
    let mut total = 0.0;
    for (card, fields) in obj {
        if !card.starts_with("card") {
            continue;
        }
        let Some(fields) = fields.as_object() else {
            continue;
        };
        for (k, val) in fields {
            let kl = k.to_ascii_lowercase();
            if kl.contains("power") && kl.contains("(w)") {
                let w = match val {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => s.trim().parse().ok(),
                    _ => None,
                };
                if let Some(w) = w {
                    total += w;
                    break; // one power figure per card
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rocm5_string_values() {
        let json = r#"{
            "card0": {"Average Graphics Package Power (W)": "41.0"},
            "card1": {"Average Graphics Package Power (W)": "203.5"},
            "system": {"Driver version": "6.3.6"}
        }"#;
        assert_eq!(parse_power_watts(json), 244.5);
    }

    #[test]
    fn parses_rocm6_socket_power() {
        let json = r#"{
            "card0": {"Current Socket Graphics Package Power (W)": "97.0"}
        }"#;
        assert_eq!(parse_power_watts(json), 97.0);
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(parse_power_watts(""), 0.0);
        assert_eq!(parse_power_watts("not json"), 0.0);
        assert_eq!(parse_power_watts("{}"), 0.0);
    }
}
