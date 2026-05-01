//! Power readers feeding Cognitora's energy-aware scheduling.
//!
//! Three sources, in preference order:
//!
//! 1. **Redfish** — vendor-neutral DC out-of-band API, reports total chassis
//!    power and per-PSU draw. Best signal when available.
//! 2. **IPMI** — `ipmitool sdr` parsed by a tiny shell-out helper. Used as
//!    a fallback when Redfish isn't reachable.
//! 3. **NVML (DCGM)** — per-GPU power draw via `nvml-wrapper`. Always read
//!    when an NVIDIA GPU is present, blended with the chassis number to
//!    derive `gpu_share`.
//!
//! `cgn-metrics` polls these readers on a configurable interval and
//! exports `cgn_power_watts{component=...}` plus derived gauges that the
//! router consumes through its `power` score component.

#![forbid(unsafe_code)]

pub mod redfish;
pub mod nvml;

use async_trait::async_trait;
use cgn_core::Result;

/// Single power reading at a moment in time.
#[derive(Debug, Clone)]
pub struct PowerSample {
    pub watts:     f64,
    pub component: &'static str, // "chassis" | "gpu" | "psu"
    pub at_unix:   i64,
}

/// Provider trait.
#[async_trait]
pub trait PowerReader: Send + Sync {
    fn name(&self) -> &'static str;
    async fn sample(&self) -> Result<Vec<PowerSample>>;
}
