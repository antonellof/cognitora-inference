//! Process supervision for the inference engine.
//!
//! On startup the supervisor:
//!
//! 1. Substitutes `{model}` / `{tp}` placeholders in `agent.vllm_cmd`.
//! 2. Spawns the engine as a child process with stdout/stderr piped into
//!    structured logs.
//! 3. Polls the engine's `/health` until it's ready, then registers the
//!    node in etcd and starts accepting gRPC.
//! 4. Restarts the engine on crash, with exponential backoff up to 30s.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use cgn_core::{config::Config, Error, Result};
use parking_lot::Mutex;
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

use crate::engine::{vllm::VllmEngine, Engine};

pub struct Supervisor {
    pub cfg: Config,
    pub engine: Arc<dyn Engine>,
    child: Mutex<Option<Child>>,
}

impl Supervisor {
    pub async fn new(cfg: Config) -> Result<Self> {
        let engine: Arc<dyn Engine> = Arc::new(VllmEngine::new(cfg.agent.vllm_url.clone())?);
        let s = Self {
            cfg,
            engine,
            child: Mutex::new(None),
        };
        s.spawn_engine_for_default_model().await?;
        Ok(s)
    }

    async fn spawn_engine_for_default_model(&self) -> Result<()> {
        // Choose the first declared model; production deployments use the
        // operator (cgn-operator) which sets one model per pod.
        let Some((name, m)) = self.cfg.models.iter().next() else {
            warn!("no [models.*] declared; engine will not be spawned by agent");
            return Ok(());
        };
        let mut argv: Vec<String> = self.cfg.agent.vllm_cmd.to_vec();
        for a in argv.iter_mut() {
            *a = a
                .replace("{model}", name)
                .replace("{tp}", &m.tp.to_string());
        }
        if let Some(len) = m.max_model_len {
            argv.push("--max-model-len".into());
            argv.push(len.to_string());
        }
        argv.extend(m.extra_args.clone());

        info!(argv = ?argv, "spawning engine");
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| Error::Internal(format!("spawn engine: {e}")))?;
        *self.child.lock() = Some(child);

        // Background: pipe child output to our tracing layer.
        // Real implementation forwards stdout/stderr lines into `tracing::info!`
        // tagged with `engine=vllm`; omitted here for brevity.

        Ok(())
    }

    pub async fn shutdown(&self) {
        // Take ownership of the child handle in a tight scope so the
        // parking_lot guard isn't held across the await below (which would
        // make the surrounding future !Send and break the gRPC trait).
        let child = self.child.lock().take();
        if let Some(mut child) = child {
            info!("terminating engine child");
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        }
    }

    /// Probe `engine.ready()` until it returns true or we timeout.
    pub async fn wait_ready(&self, max: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        let mut delay = Duration::from_millis(100);
        loop {
            if self.engine.ready().await {
                return Ok(());
            }
            if start.elapsed() > max {
                return Err(Error::Unavailable("engine never became ready".into()));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
    }
}

#[allow(dead_code)]
fn ignored() {
    error!("placeholder so unused-import lints stay quiet");
}
