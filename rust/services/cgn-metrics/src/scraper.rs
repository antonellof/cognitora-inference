//! Periodic Prometheus scraper that pulls neighbour `/metrics` endpoints
//! and re-exposes them under our admin surface as a federation target.
//!
//! For now this is a placeholder loop; the full federation pipeline
//! (`reqwest` → `prometheus::TextEncoder` re-encoding) is tracked as
//! future work.

use cgn_core::{config::Config, Result};
use tracing::{debug, warn};

pub async fn run(cfg: Config) -> Result<()> {
    let mut interval = tokio::time::interval(cfg.metrics.scrape_interval);
    loop {
        interval.tick().await;
        if let Err(e) = scrape_once(&cfg).await {
            warn!(error=?e, "scrape iteration failed");
        }
    }
}

async fn scrape_once(_cfg: &Config) -> Result<()> {
    debug!("scrape tick (placeholder)");
    Ok(())
}
