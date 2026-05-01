//! Redfish chassis power reader.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use cgn_core::{Error, Result};
use serde::Deserialize;

use super::{PowerReader, PowerSample};

/// Reader against `<base>/redfish/v1/Chassis/{id}/Power`.
pub struct Redfish {
    base:     String,
    chassis:  String,
    auth_b64: String,
    client:   reqwest::Client,
}

impl Redfish {
    pub fn new(base_url: &str, chassis_id: &str, user: &str, password: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true) // BMCs commonly use self-signed certs
            .build()
            .map_err(|e| Error::Internal(format!("reqwest: {e}")))?;

        let creds = format!("{user}:{password}");
        let b64 = base64::engine::general_purpose::STANDARD.encode(creds);

        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            chassis: chassis_id.to_string(),
            auth_b64: format!("Basic {b64}"),
            client,
        })
    }
}

#[async_trait]
impl PowerReader for Redfish {
    fn name(&self) -> &'static str { "redfish" }

    async fn sample(&self) -> Result<Vec<PowerSample>> {
        #[derive(Deserialize)]
        struct PowerDoc {
            #[serde(rename = "PowerControl", default)]
            power_control: Vec<Pc>,
        }
        #[derive(Deserialize)]
        struct Pc {
            #[serde(rename = "PowerConsumedWatts", default)]
            power_consumed_watts: Option<f64>,
        }

        let url = format!("{}/redfish/v1/Chassis/{}/Power", self.base, self.chassis);
        let resp = self.client.get(&url)
            .header(reqwest::header::AUTHORIZATION, &self.auth_b64)
            .header(reqwest::header::ACCEPT, "application/json")
            .send().await
            .map_err(|e| Error::Unavailable(format!("redfish get: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Unavailable(format!("redfish status: {}", resp.status())));
        }
        let doc: PowerDoc = resp.json().await
            .map_err(|e| Error::InvalidArgument(format!("redfish json: {e}")))?;

        let watts: f64 = doc.power_control.iter()
            .filter_map(|p| p.power_consumed_watts)
            .sum();

        let now = chrono::Utc::now().timestamp();
        Ok(vec![PowerSample { watts, component: "chassis", at_unix: now }])
    }
}
