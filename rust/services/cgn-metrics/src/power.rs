//! Power-collection loop that updates `cgn_power_watts{component=...}`
//! gauges from Redfish + NVML + ROCm, used by the router's energy-aware
//! score.

use std::time::Duration;

use cgn_core::{config::Config, Result};
use cgn_power::{nvml::Nvml, redfish::Redfish, rocm::Rocm, PowerReader};
use tracing::warn;

pub async fn run(cfg: Config) -> Result<()> {
    let chassis_w = cgn_telemetry::gauge!(
        "cgn_power_watts_chassis",
        "Whole-chassis power consumption in watts (Redfish)"
    );
    let gpu_w = cgn_telemetry::gauge!(
        "cgn_power_watts_gpu",
        "Sum of per-GPU power draw in watts (NVML on NVIDIA, rocm-smi on AMD)"
    );

    let redfish = cfg.metrics.redfish_url.as_ref().and_then(|url| {
        let user = cfg.metrics.redfish_user.as_deref().unwrap_or("");
        let pass = cfg.metrics.redfish_password.as_deref().unwrap_or("");
        Redfish::new(url, "1", user, pass).ok()
    });
    let nvml = Nvml::new();
    let rocm = Rocm::new();

    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        if let Some(rf) = &redfish {
            match rf.sample().await {
                Ok(samples) => {
                    for s in samples {
                        chassis_w.set(s.watts as i64);
                    }
                }
                Err(e) => warn!(error=?e, "redfish sample"),
            }
        }
        // A host realistically has one GPU vendor; the absent vendor's
        // reader returns an empty sample list, so summing is safe.
        let mut gpu_total = 0.0;
        for reader in [&nvml as &dyn PowerReader, &rocm as &dyn PowerReader] {
            match reader.sample().await {
                Ok(samples) => {
                    for s in samples {
                        gpu_total += s.watts;
                    }
                }
                Err(e) => warn!(reader = reader.name(), error=?e, "power sample"),
            }
        }
        gpu_w.set(gpu_total as i64);
    }
}
