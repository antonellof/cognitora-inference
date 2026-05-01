//! `cgn-ctl bench …`
//!
//! Lightweight load generator for smoke tests. Real perf gates live in
//! `tests/perf/` (criterion-driven) — this is the operator's quick check.

use std::time::Instant;

use cgn_core::Result;
use clap::{Args as ClapArgs, ValueEnum};
use tracing::info;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// What to benchmark.
    #[arg(long, value_enum, default_value_t = What::Chat)]
    pub what: What,

    /// Target endpoint (e.g. http://localhost:8080).
    #[arg(long, default_value = "http://localhost:8080")]
    pub endpoint: String,

    /// Concurrency.
    #[arg(long, default_value_t = 4)]
    pub concurrency: u32,

    /// Total requests.
    #[arg(short = 'n', long, default_value_t = 64)]
    pub requests: u32,

    /// Model to use.
    #[arg(long, default_value = "llama3-8b")]
    pub model: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum What {
    Chat,
    Embed,
    Health,
}

pub async fn run(args: Args) -> Result<()> {
    info!(?args.what, endpoint = %args.endpoint, "bench");
    let started = Instant::now();
    match args.what {
        What::Health => health(&args).await?,
        What::Chat => chat(&args).await?,
        What::Embed => info!("embed bench: not yet implemented"),
    }
    info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "bench done"
    );
    Ok(())
}

async fn health(args: &Args) -> Result<()> {
    let url = format!("{}/healthz", args.endpoint.trim_end_matches('/'));
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| cgn_core::Error::Unavailable(format!("get {url}: {e}")))?;
    info!(status = resp.status().as_u16(), "/healthz");
    Ok(())
}

async fn chat(args: &Args) -> Result<()> {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    let url = format!(
        "{}/v1/chat/completions",
        args.endpoint.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": args.model,
        "messages": [{"role": "user", "content": "Hello!"}],
        "stream": false,
        "max_tokens": 32,
    });

    let client = Arc::new(
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| cgn_core::Error::Internal(format!("reqwest: {e}")))?,
    );
    let ok = Arc::new(AtomicU32::new(0));
    let err = Arc::new(AtomicU32::new(0));
    let total_ms = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(args.concurrency as usize);
    let per_worker = (args.requests / args.concurrency).max(1);

    for _ in 0..args.concurrency {
        let url = url.clone();
        let body = body.clone();
        let client = client.clone();
        let ok = ok.clone();
        let err = err.clone();
        let total_ms = total_ms.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..per_worker {
                let t0 = Instant::now();
                match client.post(&url).json(&body).send().await {
                    Ok(r) if r.status().is_success() => {
                        ok.fetch_add(1, Ordering::Relaxed);
                        total_ms.fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                    Ok(r) => {
                        err.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(status = r.status().as_u16(), "chat non-2xx");
                    }
                    Err(e) => {
                        err.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(error=?e, "chat error");
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let ok_n = ok.load(Ordering::Relaxed);
    let err_n = err.load(Ordering::Relaxed);
    let avg_ms = if ok_n > 0 {
        total_ms.load(Ordering::Relaxed) / ok_n as u64
    } else {
        0
    };
    info!(ok = ok_n, err = err_n, avg_ms, "chat bench complete");
    Ok(())
}
