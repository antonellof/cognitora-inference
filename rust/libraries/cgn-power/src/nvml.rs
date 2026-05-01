//! NVML (NVIDIA Management Library) power reader.

use async_trait::async_trait;
use cgn_core::{Error, Result};
use parking_lot::Mutex;
use std::sync::OnceLock;

use super::{PowerReader, PowerSample};

/// Reader that reports per-GPU `power.draw` summed across the system.
///
/// On a host without NVML installed (CPU-only dev box) this becomes a no-op
/// that returns an empty list — the metrics pipeline still exposes the
/// chassis number from Redfish.
pub struct Nvml {
    inner: Mutex<Option<nvml_wrapper::Nvml>>,
}

impl Nvml {
    pub fn new() -> Self {
        static INIT: OnceLock<Result<()>> = OnceLock::new();
        INIT.get_or_init(|| {
            // Probe once to log a single warning if NVML isn't present.
            match nvml_wrapper::Nvml::init() {
                Ok(_) => Ok(()),
                Err(e) => {
                    tracing::warn!(error=?e, "NVML not available; per-GPU power disabled");
                    Err(Error::Unavailable("nvml".into()))
                }
            }
        });
        Self {
            inner: Mutex::new(nvml_wrapper::Nvml::init().ok()),
        }
    }
}

impl Default for Nvml {
    fn default() -> Self {
        Self::new()
    }
}

// `parking_lot::Mutex` requires explicit drop semantics for the trait
// object; we keep the data inside an `Option`.

#[async_trait]
impl PowerReader for Nvml {
    fn name(&self) -> &'static str {
        "nvml"
    }

    async fn sample(&self) -> Result<Vec<PowerSample>> {
        let nvml_opt = {
            let mut g = self.inner.lock();
            (*g).take()
        };

        let nvml = match nvml_opt {
            Some(n) => n,
            None => return Ok(vec![]),
        };

        let count = nvml.device_count().unwrap_or(0);
        let mut total = 0.0;
        for i in 0..count {
            if let Ok(dev) = nvml.device_by_index(i) {
                if let Ok(milli) = dev.power_usage() {
                    total += milli as f64 / 1000.0;
                }
            }
        }

        // Restore handle.
        *self.inner.lock() = Some(nvml);

        let now = chrono::Utc::now().timestamp();
        Ok(vec![PowerSample {
            watts: total,
            component: "gpu",
            at_unix: now,
        }])
    }
}
