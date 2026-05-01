//! Power-collection loop that updates `cgn_power_watts{component=...}`
//! gauges from Redfish + NVML, used by the router's energy-aware score.

use std::time::Duration;

use cgn_core::{config::Config, Result};
use cgn_power::{nvml::Nvml, redfish::Redfish, PowerReader};
use tracing::warn;

pub async fn run(cfg: Config) -> Result<()> {
    let chassis_w = cgn_telemetry::gauge!(
        "cgn_power_watts_chassis",
        "Whole-chassis power consumption in watts (Redfish)"
    );
    let gpu_w = cgn_telemetry::gauge!(
        "cgn_power_watts_gpu",
        "Sum of per-GPU power draw in watts (NVML)"
    );

    let redfish = cfg.metrics.redfish_url.as_ref().and_then(|url| {
        let user = cfg.metrics.redfish_user.as_deref().unwrap_or("");
        let pass = cfg.metrics.redfish_password.as_deref().unwrap_or("");
        Redfish::new(url, "1", user, pass).ok()
    });
    let nvml = Nvml::new();

    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        if let Some(rf) = &redfish {
            match rf.sample().await {
                Ok(samples) => for s in samples { chassis_w.set(s.watts as i64); },
                Err(e) => warn!(error=?e, "redfish sample"),
            }
        }
        match nvml.sample().await {
            Ok(samples) => for s in samples { gpu_w.set(s.watts as i64); },
            Err(e) => warn!(error=?e, "nvml sample"),
        }
    }
}
