//! Federation scraper.
//!
//! Periodically pulls the configured `[metrics].scrape_targets`, decorates
//! every metric line with a `cgn_target = "<name>"` label, and caches the
//! concatenated body. The cached body is served from the binary's
//! `/federate` endpoint so an upstream Prometheus can scrape *one* URL
//! and still see per-target labels.
//!
//! The decoration is text-level (we don't re-parse into the protobuf
//! `MetricFamily` model), which keeps the implementation small and
//! tolerant of any client library's output. The Prometheus exposition
//! format (`text/plain; version=0.0.4`) is well enough specified for a
//! line-by-line transformation:
//!
//! * `# HELP …` and `# TYPE …` lines pass through unchanged.
//! * `<name>{<labels>} <value>` gets `cgn_target="<n>"` injected into
//!   the existing label set.
//! * `<name> <value>` (no labels) gets a new `{cgn_target="<n>"}` block.
//!
//! Targets that 4xx, 5xx, time out, or return invalid UTF-8 are skipped
//! for that scrape iteration and counted in
//! `cgn_metrics_scrape_errors_total`.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use cgn_core::{config::Config, Result};
use prometheus::IntCounterVec;
use tracing::{debug, info, warn};

/// Cached federated exposition body. Replaced atomically every tick.
pub struct Cache(ArcSwap<String>);

impl Cache {
    pub fn new() -> Self {
        Self(ArcSwap::from_pointee(String::new()))
    }
    pub fn snapshot(&self) -> Arc<String> {
        self.0.load_full()
    }
    fn store(&self, s: String) {
        self.0.store(Arc::new(s));
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the scrape loop. Returns immediately once the first scrape is
/// complete so the federation endpoint serves something useful right
/// away instead of an empty body for the first `scrape_interval`.
pub async fn run(cfg: Config, cache: Arc<Cache>) -> Result<()> {
    let errors = scrape_errors_metric();
    if cfg.metrics.scrape_targets.is_empty() {
        info!("no [metrics].scrape_targets configured; federation disabled");
        // Idle the task — main task lifetime tracks the binary, so we just
        // sleep here and never block startup.
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error=?e, "scraper: building reqwest client failed");
            return Ok(());
        }
    };

    info!(
        targets = cfg.metrics.scrape_targets.len(),
        interval_s = cfg.metrics.scrape_interval.as_secs(),
        "metrics federation scraper running"
    );

    // First scrape immediately so /federate has data; then on the configured
    // cadence.
    let mut tick = tokio::time::interval(cfg.metrics.scrape_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let body = scrape_once(&client, &cfg, &errors).await;
        cache.store(body);
    }
}

async fn scrape_once(client: &reqwest::Client, cfg: &Config, errors: &IntCounterVec) -> String {
    let mut handles = Vec::with_capacity(cfg.metrics.scrape_targets.len());
    for tgt in &cfg.metrics.scrape_targets {
        let client = client.clone();
        let name = tgt.name.clone();
        let url = tgt.url.clone();
        let errors = errors.clone();
        handles.push(tokio::spawn(async move {
            match fetch(&client, &url).await {
                Ok(body) => {
                    debug!(%name, %url, bytes = body.len(), "scrape ok");
                    Some(decorate_with_target(&body, &name))
                }
                Err(e) => {
                    warn!(%name, %url, error=?e, "scrape failed");
                    errors.with_label_values(&[&name]).inc();
                    None
                }
            }
        }));
    }

    let mut out = String::with_capacity(64 * 1024);
    for h in handles {
        if let Ok(Some(body)) = h.await {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&body);
        }
    }
    out
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<String> {
    use cgn_core::Error;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Unavailable(format!("get {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Unavailable(format!(
            "get {url}: status {}",
            resp.status()
        )));
    }
    resp.text()
        .await
        .map_err(|e| Error::Internal(format!("read body {url}: {e}")))
}

/// Insert `cgn_target="<name>"` into every metric line of a Prometheus
/// exposition body. `# HELP` / `# TYPE` lines pass through unchanged so
/// the output is still valid 0.0.4-format text.
pub fn decorate_with_target(body: &str, target: &str) -> String {
    let mut out = String::with_capacity(body.len() + 64 * 1024);
    out.push_str(&format!("# cgn-metrics: federated from target {target}\n"));
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        match decorate_one(line, target) {
            Some(decorated) => out.push_str(&decorated),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// Inject `cgn_target` into a single `name{labels} value` (or `name value`)
/// line. Returns `None` when the line is malformed; the caller falls back
/// to passing the raw line through.
fn decorate_one(line: &str, target: &str) -> Option<String> {
    let label_kv = format!("cgn_target=\"{}\"", escape_label_value(target));

    if let Some(open) = line.find('{') {
        let close = line.find('}')?;
        if close < open {
            return None;
        }
        let inside = &line[open + 1..close];
        let mut out = String::with_capacity(line.len() + label_kv.len() + 2);
        out.push_str(&line[..=open]); // up to and including '{'
        if !inside.trim().is_empty() {
            out.push_str(inside);
            out.push(',');
        }
        out.push_str(&label_kv);
        out.push_str(&line[close..]); // from '}' to end
        return Some(out);
    }

    // No labels — split on the first whitespace, e.g. `metric_name 1.0`.
    let (name, rest) = line.split_once(char::is_whitespace)?;
    Some(format!("{name}{{{label_kv}}} {rest}"))
}

fn escape_label_value(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn scrape_errors_metric() -> IntCounterVec {
    use prometheus::Opts;
    let opts = Opts::new(
        "cgn_metrics_scrape_errors_total",
        "Number of failed `[metrics].scrape_targets` fetches per target.",
    );
    let cv = IntCounterVec::new(opts, &["target"]).expect("valid metric");
    let _ = cgn_telemetry::registry().register(Box::new(cv.clone()));
    cv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorate_passes_through_help_and_type() {
        let body = "\
# HELP foo The foo gauge.
# TYPE foo gauge
foo 1
";
        let out = decorate_with_target(body, "router");
        assert!(out.contains("# HELP foo The foo gauge."));
        assert!(out.contains("# TYPE foo gauge"));
        assert!(out.contains("foo{cgn_target=\"router\"} 1"));
    }

    #[test]
    fn decorate_injects_into_existing_labels() {
        let body = r#"http_requests_total{method="get",code="200"} 42"#;
        let out = decorate_with_target(body, "agent-1");
        assert!(
            out.contains(r#"http_requests_total{method="get",code="200",cgn_target="agent-1"} 42"#),
            "out = {out}"
        );
    }

    #[test]
    fn decorate_handles_empty_label_set() {
        let body = "ready{} 1";
        let out = decorate_with_target(body, "kv");
        assert!(out.contains(r#"ready{cgn_target="kv"} 1"#), "out = {out}");
    }

    #[test]
    fn decorate_escapes_quotes_and_backslashes() {
        let body = "ok 1";
        let out = decorate_with_target(body, r#"name with " and \ "#);
        assert!(
            out.contains(r#"ok{cgn_target="name with \" and \\ "} 1"#),
            "out = {out}"
        );
    }

    #[test]
    fn decorate_skips_blank_lines() {
        let body = "ok 1\n\nok2 2\n";
        let out = decorate_with_target(body, "x");
        assert!(out.contains(r#"ok{cgn_target="x"} 1"#));
        assert!(out.contains(r#"ok2{cgn_target="x"} 2"#));
    }
}
