//! Engine telemetry scraper.
//!
//! Pulls the engine's Prometheus `/metrics` endpoint and extracts the
//! signals the router's score function consumes: queue depth and KV-cache
//! occupancy. Without these the `load` and `capacity` terms of the routing
//! score are inert (they were hardcoded to zero before 0.6).
//!
//! Metric names per engine:
//!
//! | engine  | queue depth                                  | KV occupancy                 |
//! |---------|----------------------------------------------|------------------------------|
//! | vllm    | `vllm:num_requests_waiting` + `…_running`    | `vllm:gpu_cache_usage_perc`  |
//! | sglang  | `sglang:num_queue_reqs` + `…num_running_reqs`| `sglang:token_usage`         |
//! | others  | not exposed — zeros are reported honestly    | —                            |
//!
//! vLLM and SGLang report cache occupancy as a fraction, not a block
//! count. We surface it through the existing `free_blocks / total_blocks`
//! wire fields using a fixed synthetic denominator
//! ([`SYNTHETIC_TOTAL_BLOCKS`]); the router only ever consumes the
//! *ratio*, so the scale cancels out in the score.

use std::time::Duration;

use tracing::debug;

/// Synthetic denominator for engines that report cache occupancy as a
/// fraction. The router's capacity signal is `free / total`, so any
/// fixed scale is equivalent.
pub const SYNTHETIC_TOTAL_BLOCKS: u32 = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EngineStats {
    pub queue_depth: u32,
    pub free_blocks: u32,
    pub total_blocks: u32,
}

/// Scrape `<base_url>/metrics` and parse engine-specific gauges.
/// Returns `None` when the engine has no metrics endpoint (llama.cpp,
/// mlx_lm, cgn-infer, generic openai_compat) or the scrape fails —
/// callers fall back to zeros, which is the honest signal.
pub async fn scrape(engine_kind: &str, base_url: &str) -> Option<EngineStats> {
    if !has_prometheus_metrics(engine_kind) {
        return None;
    }
    let url = format!("{}/metrics", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let text = client.get(&url).send().await.ok()?.text().await.ok()?;
    let stats = parse_metrics(engine_kind, &text);
    debug!(?stats, engine = engine_kind, "engine telemetry scraped");
    stats
}

/// Engines whose bundled server exposes a Prometheus `/metrics` route.
pub fn has_prometheus_metrics(engine_kind: &str) -> bool {
    matches!(engine_kind, "vllm" | "sglang")
}

/// Parse the Prometheus text exposition format for the metrics we need.
pub fn parse_metrics(engine_kind: &str, text: &str) -> Option<EngineStats> {
    match engine_kind {
        "vllm" => {
            let waiting = find_metric(text, "vllm:num_requests_waiting");
            let running = find_metric(text, "vllm:num_requests_running");
            let usage = find_metric(text, "vllm:gpu_cache_usage_perc");
            build(waiting, running, usage)
        }
        "sglang" => {
            let waiting = find_metric(text, "sglang:num_queue_reqs");
            let running = find_metric(text, "sglang:num_running_reqs");
            let usage = find_metric(text, "sglang:token_usage");
            build(waiting, running, usage)
        }
        _ => None,
    }
}

fn build(waiting: Option<f64>, running: Option<f64>, usage: Option<f64>) -> Option<EngineStats> {
    // At least one signal must be present for the scrape to count.
    if waiting.is_none() && running.is_none() && usage.is_none() {
        return None;
    }
    let queue_depth = (waiting.unwrap_or(0.0) + running.unwrap_or(0.0)).max(0.0) as u32;
    let (free_blocks, total_blocks) = match usage {
        Some(u) => {
            let used = u.clamp(0.0, 1.0);
            let free = ((1.0 - used) * SYNTHETIC_TOTAL_BLOCKS as f64).round() as u32;
            (free, SYNTHETIC_TOTAL_BLOCKS)
        }
        None => (0, 0),
    };
    Some(EngineStats {
        queue_depth,
        free_blocks,
        total_blocks,
    })
}

/// Find the value of `name` in Prometheus text format. Handles both
/// bare metrics (`name 3.0`) and labeled ones (`name{a="b"} 3.0`);
/// when multiple series share the name (e.g. one per served model),
/// values are summed for counters/queues — for our gauges a sum over
/// a single-model engine is the value itself.
fn find_metric(text: &str, name: &str) -> Option<f64> {
    let mut sum = 0.0f64;
    let mut seen = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let matches_name = match line.as_bytes().get(name.len()) {
            _ if !line.starts_with(name) => false,
            Some(b' ') | Some(b'{') => true,
            None => false, // no value on the line
            _ => false,    // longer metric name sharing the prefix
        };
        if !matches_name {
            continue;
        }
        let value_part = line.rsplit(' ').next()?;
        if let Ok(v) = value_part.parse::<f64>() {
            sum += v;
            seen = true;
        }
    }
    if seen {
        Some(sum)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VLLM_SAMPLE: &str = r#"
# HELP vllm:num_requests_running Number of requests currently running on GPU.
# TYPE vllm:num_requests_running gauge
vllm:num_requests_running{model_name="meta-llama/Llama-3.1-8B"} 3.0
# HELP vllm:num_requests_waiting Number of requests waiting to be processed.
# TYPE vllm:num_requests_waiting gauge
vllm:num_requests_waiting{model_name="meta-llama/Llama-3.1-8B"} 5.0
# HELP vllm:gpu_cache_usage_perc GPU KV-cache usage. 1 means 100 percent usage.
# TYPE vllm:gpu_cache_usage_perc gauge
vllm:gpu_cache_usage_perc{model_name="meta-llama/Llama-3.1-8B"} 0.25
"#;

    const SGLANG_SAMPLE: &str = r#"
# TYPE sglang:num_running_reqs gauge
sglang:num_running_reqs{model_name="qwen"} 2.0
# TYPE sglang:num_queue_reqs gauge
sglang:num_queue_reqs{model_name="qwen"} 1.0
# TYPE sglang:token_usage gauge
sglang:token_usage{model_name="qwen"} 0.4
"#;

    #[test]
    fn parses_vllm_metrics() {
        let s = parse_metrics("vllm", VLLM_SAMPLE).unwrap();
        assert_eq!(s.queue_depth, 8); // 5 waiting + 3 running
        assert_eq!(s.total_blocks, SYNTHETIC_TOTAL_BLOCKS);
        assert_eq!(s.free_blocks, 7_500); // (1 - 0.25) * 10_000
    }

    #[test]
    fn parses_sglang_metrics() {
        let s = parse_metrics("sglang", SGLANG_SAMPLE).unwrap();
        assert_eq!(s.queue_depth, 3);
        assert_eq!(s.free_blocks, 6_000);
        assert_eq!(s.total_blocks, SYNTHETIC_TOTAL_BLOCKS);
    }

    #[test]
    fn unknown_engine_yields_none() {
        assert!(parse_metrics("llama_cpp", VLLM_SAMPLE).is_none());
        assert!(parse_metrics("mlx", "").is_none());
    }

    #[test]
    fn missing_metrics_yield_none() {
        assert!(parse_metrics("vllm", "# nothing here\n").is_none());
    }

    #[test]
    fn bare_metric_without_labels_parses() {
        let s = parse_metrics("vllm", "vllm:num_requests_waiting 4\n").unwrap();
        assert_eq!(s.queue_depth, 4);
        assert_eq!(s.total_blocks, 0); // usage absent → no capacity claim
    }

    #[test]
    fn prefix_collision_is_not_matched() {
        // `vllm:num_requests_waiting_total` must not match `…_waiting`.
        let text = "vllm:num_requests_waiting_total 99\nvllm:num_requests_waiting 2\n";
        let s = parse_metrics("vllm", text).unwrap();
        assert_eq!(s.queue_depth, 2);
    }

    #[test]
    fn usage_clamped_to_unit_range() {
        let s = parse_metrics("vllm", "vllm:gpu_cache_usage_perc 1.7\n").unwrap();
        assert_eq!(s.free_blocks, 0);
    }
}
