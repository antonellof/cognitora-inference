//! Grid carbon intensity polling and low-priority admission gating.
//!
//! When `[carbon].enabled = true`, a background task polls a pluggable
//! intensity provider and exposes the latest reading as
//! `cgn_carbon_intensity_gco2_per_kwh{zone}`. The OpenAI gateway rejects
//! requests carrying `X-CGN-Priority: low` while intensity exceeds
//! `[carbon].intensity_threshold`.

mod provider;

use std::sync::{Arc, LazyLock};
use std::time::Instant;

use arc_swap::ArcSwap;
use cgn_core::config::{CarbonConfig, CarbonProviderKind, Config};
use cgn_core::Result;
use cgn_telemetry::prometheus::{GaugeVec, IntCounter};
use cgn_telemetry::{counter, float_gauge_vec};
use tracing::{info, warn};

pub use provider::{build_provider, IntensityProvider};

/// Request priority for carbon-aware admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPriority {
    Low,
    Normal,
    High,
}

impl RequestPriority {
    pub fn parse_header(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Normal,
        }
    }
}

/// Latest grid-intensity sample held in an `ArcSwap` for lock-free reads.
#[derive(Debug, Clone)]
pub struct CarbonSample {
    pub intensity_gco2_per_kwh: f64,
    pub zone: String,
    pub provider: String,
    pub updated_at: Instant,
    /// True when no successful poll has completed yet.
    pub stale: bool,
}

impl Default for CarbonSample {
    fn default() -> Self {
        Self {
            intensity_gco2_per_kwh: 0.0,
            zone: String::new(),
            provider: String::new(),
            updated_at: Instant::now(),
            stale: true,
        }
    }
}

#[derive(Default)]
pub struct CarbonTracker {
    sample: ArcSwap<CarbonSample>,
}

impl CarbonTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Arc<CarbonSample> {
        self.sample.load_full()
    }

    pub fn store(&self, intensity: f64, zone: &str, provider: &str) {
        let sample = Arc::new(CarbonSample {
            intensity_gco2_per_kwh: intensity,
            zone: zone.to_string(),
            provider: provider.to_string(),
            updated_at: Instant::now(),
            stale: false,
        });
        self.sample.store(sample);
        INTENSITY_GAUGE
            .with_label_values(&[zone])
            .set(intensity);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CarbonAdmission {
    Admit,
    Reject {
        intensity: f64,
        threshold: f64,
    },
}

/// Evaluate whether a request should be admitted given the current sample.
pub fn check_admission(cfg: &CarbonConfig, sample: &CarbonSample, priority: RequestPriority) -> CarbonAdmission {
    if !cfg.enabled || priority != RequestPriority::Low {
        return CarbonAdmission::Admit;
    }
    // Fail open until the first poll succeeds — availability beats carbon
    // gating on cold start or transient provider outages.
    if sample.stale {
        return CarbonAdmission::Admit;
    }
    if sample.intensity_gco2_per_kwh > cfg.intensity_threshold {
        CarbonAdmission::Reject {
            intensity: sample.intensity_gco2_per_kwh,
            threshold: cfg.intensity_threshold,
        }
    } else {
        CarbonAdmission::Admit
    }
}

static INTENSITY_GAUGE: LazyLock<GaugeVec> = LazyLock::new(|| {
    float_gauge_vec!(
        "cgn_carbon_intensity_gco2_per_kwh",
        "Latest grid carbon intensity observed by the router (gCO2/kWh).",
        &["zone"]
    )
});

static ADMISSION_REJECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    counter!(
        "cgn_router_carbon_admission_rejected_total",
        "Low-priority requests rejected because grid carbon intensity exceeded the configured threshold."
    )
});

pub fn record_rejection() {
    ADMISSION_REJECTED.inc();
}

pub fn warm_up_metrics() {
    LazyLock::force(&INTENSITY_GAUGE);
    LazyLock::force(&ADMISSION_REJECTED);
}

/// Spawn the background poller. Returns immediately.
pub fn spawn(state: Arc<crate::state::SharedState>) {
    warm_up_metrics();
    let cfg = state.cfg.carbon.clone();
    if !cfg.enabled {
        info!("carbon-aware admission disabled");
        return;
    }

    let provider = match build_provider(&cfg) {
        Ok(p) => p,
        Err(e) => {
            warn!(error=?e, "carbon provider init failed; admission stays fail-open");
            return;
        }
    };

    info!(
        provider = provider.name(),
        zone = %cfg.zone,
        threshold = cfg.intensity_threshold,
        "carbon-aware admission enabled"
    );

    tokio::spawn(async move {
        let interval = cfg.poll_interval;
        let mut tick = tokio::time::interval(interval);
        // Poll immediately on startup.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = poll_once(&state.cfg, state.carbon.clone(), provider.as_ref()).await {
                warn!(error=?e, "carbon intensity poll failed");
            }
        }
    });
}

async fn poll_once(
    cfg: &Config,
    tracker: Arc<CarbonTracker>,
    provider: &dyn IntensityProvider,
) -> Result<()> {
    let intensity = provider.fetch().await?;
    let zone = if cfg.carbon.zone.is_empty() {
        "static"
    } else {
        cfg.carbon.zone.as_str()
    };
    tracker.store(intensity, zone, provider.name());
    tracing::debug!(
        intensity,
        zone,
        provider = provider.name(),
        "carbon intensity updated"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(threshold: f64) -> CarbonConfig {
        CarbonConfig {
            enabled: true,
            provider: CarbonProviderKind::Static,
            static_intensity: 500.0,
            intensity_threshold: threshold,
            ..Default::default()
        }
    }

    fn fresh_sample(intensity: f64) -> CarbonSample {
        CarbonSample {
            intensity_gco2_per_kwh: intensity,
            zone: "test".into(),
            provider: "static".into(),
            updated_at: Instant::now(),
            stale: false,
        }
    }

    #[test]
    fn disabled_always_admits() {
        let mut c = cfg(100.0);
        c.enabled = false;
        assert_eq!(
            check_admission(&c, &fresh_sample(999.0), RequestPriority::Low),
            CarbonAdmission::Admit
        );
    }

    #[test]
    fn normal_priority_always_admits() {
        let c = cfg(100.0);
        assert_eq!(
            check_admission(&c, &fresh_sample(999.0), RequestPriority::Normal),
            CarbonAdmission::Admit
        );
    }

    #[test]
    fn low_priority_rejected_above_threshold() {
        let c = cfg(400.0);
        assert_eq!(
            check_admission(&c, &fresh_sample(500.0), RequestPriority::Low),
            CarbonAdmission::Reject {
                intensity: 500.0,
                threshold: 400.0,
            }
        );
    }

    #[test]
    fn low_priority_admitted_below_threshold() {
        let c = cfg(400.0);
        assert_eq!(
            check_admission(&c, &fresh_sample(350.0), RequestPriority::Low),
            CarbonAdmission::Admit
        );
    }

    #[test]
    fn stale_sample_fails_open() {
        let c = cfg(100.0);
        assert_eq!(
            check_admission(&c, &CarbonSample::default(), RequestPriority::Low),
            CarbonAdmission::Admit
        );
    }

    #[test]
    fn static_provider_polls() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cfg = CarbonConfig {
                enabled: true,
                provider: CarbonProviderKind::Static,
                static_intensity: 275.5,
                ..Default::default()
            };
            let provider = build_provider(&cfg).unwrap();
            let tracker = Arc::new(CarbonTracker::new());
            poll_once(
                &Config {
                    carbon: cfg,
                    ..Default::default()
                },
                tracker.clone(),
                provider.as_ref(),
            )
            .await
            .unwrap();
            let snap = tracker.snapshot();
            assert!(!snap.stale);
            assert!((snap.intensity_gco2_per_kwh - 275.5).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn priority_header_parsing() {
        assert_eq!(RequestPriority::parse_header("low"), RequestPriority::Low);
        assert_eq!(RequestPriority::parse_header("HIGH"), RequestPriority::High);
        assert_eq!(RequestPriority::parse_header("nope"), RequestPriority::Normal);
    }
}
