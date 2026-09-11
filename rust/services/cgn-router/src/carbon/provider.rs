//! Pluggable grid carbon intensity providers.

use std::sync::Arc;

use async_trait::async_trait;
use cgn_core::config::{CarbonConfig, CarbonProviderKind};
use cgn_core::{Error, Result};
use serde::Deserialize;

#[async_trait]
pub trait IntensityProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn fetch(&self) -> Result<f64>;
}

pub fn build_provider(cfg: &CarbonConfig) -> Result<Arc<dyn IntensityProvider>> {
    match cfg.provider {
        CarbonProviderKind::Static => Ok(Arc::new(StaticProvider {
            intensity: cfg.static_intensity,
        })),
        CarbonProviderKind::ElectricityMaps => {
            if cfg.api_token.is_empty() {
                return Err(Error::Config(
                    "[carbon].api_token is required for provider electricitymaps".into(),
                ));
            }
            Ok(Arc::new(ElectricityMapsProvider {
                client: reqwest::Client::new(),
                zone: cfg.zone.clone(),
                token: cfg.api_token.clone(),
            }))
        }
        CarbonProviderKind::WattTime => {
            if cfg.api_token.is_empty() {
                return Err(Error::Config(
                    "[carbon].api_token is required for provider watttime".into(),
                ));
            }
            Ok(Arc::new(WattTimeProvider {
                client: reqwest::Client::new(),
                region: cfg.zone.clone(),
                token: cfg.api_token.clone(),
            }))
        }
    }
}

pub struct StaticProvider {
    pub intensity: f64,
}

#[async_trait]
impl IntensityProvider for StaticProvider {
    fn name(&self) -> &'static str {
        "static"
    }

    async fn fetch(&self) -> Result<f64> {
        Ok(self.intensity)
    }
}

pub struct ElectricityMapsProvider {
    client: reqwest::Client,
    zone: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct ElectricityMapsResponse {
    #[serde(alias = "carbonIntensity")]
    carbon_intensity: f64,
}

#[async_trait]
impl IntensityProvider for ElectricityMapsProvider {
    fn name(&self) -> &'static str {
        "electricitymaps"
    }

    async fn fetch(&self) -> Result<f64> {
        let url = format!(
            "https://api.electricitymap.org/v3/carbon-intensity/latest?zone={}",
            self.zone
        );
        let resp = self
            .client
            .get(&url)
            .header("auth-token", &self.token)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("electricitymaps request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Internal(format!(
                "electricitymaps HTTP {status}: {body}"
            )));
        }
        let parsed: ElectricityMapsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Internal(format!("electricitymaps decode: {e}")))?;
        Ok(parsed.carbon_intensity)
    }
}

pub struct WattTimeProvider {
    client: reqwest::Client,
    region: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct WattTimePoint {
    #[serde(alias = "marginal")]
    value: f64,
}

#[async_trait]
impl IntensityProvider for WattTimeProvider {
    fn name(&self) -> &'static str {
        "watttime"
    }

    async fn fetch(&self) -> Result<f64> {
        let url = format!(
            "https://api.watttime.org/v3/region-from-loc?region={}",
            self.region
        );
        // WattTime v3 index endpoint: marginal emissions for the region.
        let index_url = format!(
            "https://api.watttime.org/v3/signal-index?region={}&signal_type=co2_moer",
            self.region
        );
        let resp = self
            .client
            .get(&index_url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("watttime request: {e}")))?;
        if !resp.status().is_success() {
            // Some accounts use region lookup first; fall back to direct region.
            let fallback = self
                .client
                .get(&url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| Error::Internal(format!("watttime region lookup: {e}")))?;
            if !fallback.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::Internal(format!(
                    "watttime HTTP {status}: {body}"
                )));
            }
            let region: serde_json::Value = fallback
                .json()
                .await
                .map_err(|e| Error::Internal(format!("watttime region decode: {e}")))?;
            let region_code = region
                .get("region")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.region);
            let index_url = format!(
                "https://api.watttime.org/v3/signal-index?region={region_code}&signal_type=co2_moer"
            );
            let resp = self
                .client
                .get(&index_url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| Error::Internal(format!("watttime index: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::Internal(format!(
                    "watttime HTTP {status}: {body}"
                )));
            }
            let points: Vec<WattTimePoint> = resp
                .json()
                .await
                .map_err(|e| Error::Internal(format!("watttime decode: {e}")))?;
            return points
                .first()
                .map(|p| p.value)
                .ok_or_else(|| Error::Internal("watttime returned empty index".into()));
        }
        let points: Vec<WattTimePoint> = resp
            .json()
            .await
            .map_err(|e| Error::Internal(format!("watttime decode: {e}")))?;
        points
            .first()
            .map(|p| p.value)
            .ok_or_else(|| Error::Internal("watttime returned empty index".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_provider_returns_configured_value() {
        let p = StaticProvider { intensity: 123.4 };
        assert!((p.fetch().await.unwrap() - 123.4).abs() < f64::EPSILON);
    }

    #[test]
    fn electricitymaps_requires_token() {
        let cfg = CarbonConfig {
            enabled: true,
            provider: CarbonProviderKind::ElectricityMaps,
            zone: "DE".into(),
            ..Default::default()
        };
        assert!(build_provider(&cfg).is_err());
    }
}
