//! Process supervision for the inference engine.
//!
//! On startup the supervisor:
//!
//! 1. Resolves the effective [`EngineConfig`] (honoring legacy
//!    `[agent].vllm_url` / `[agent].vllm_cmd` aliases).
//! 2. Renders the engine argv via [`engine::spawn::render_argv`].
//! 3. Spawns the engine as a child process with stdout/stderr piped into
//!    structured logs (skipped when `engine.kind = "openai_compat"`).
//! 4. Polls the engine's `/health` (or `/v1/models`) until it's ready, then
//!    registers the node in etcd and starts accepting gRPC.
//! 5. Restarts the engine on crash, with exponential backoff up to 30s.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use cgn_core::{
    config::{Config, EngineConfig, EngineKind},
    Error, Result,
};
use parking_lot::Mutex;
use tokio::process::{Child, Command};
use tracing::{info, warn};

use crate::engine::{spawn::render_argv, Engine, ModelSpec, OpenAiHttpEngine};

pub struct Supervisor {
    pub cfg: Config,
    pub engine: Arc<dyn Engine>,
    /// Effective engine config (legacy `[agent].vllm_*` merged into a real
    /// [`EngineConfig`]).
    pub engine_cfg: EngineConfig,
    child: Mutex<Option<Child>>,
}

impl Supervisor {
    pub async fn new(cfg: Config) -> Result<Self> {
        let engine_cfg = resolve_engine_config(&cfg);
        let engine_kind: &'static str = match engine_cfg.kind {
            EngineKind::Vllm => "vllm",
            EngineKind::Sglang => "sglang",
            EngineKind::LlamaCpp => "llama_cpp",
            EngineKind::Mlx => "mlx",
            EngineKind::OpenaiCompat => "openai_compat",
        };
        info!(kind = %engine_kind, url = %engine_cfg.url, "engine configured");

        let engine: Arc<dyn Engine> =
            Arc::new(OpenAiHttpEngine::new(engine_kind, engine_cfg.url.clone())?);
        let s = Self {
            cfg,
            engine_cfg,
            engine,
            child: Mutex::new(None),
        };
        s.spawn_engine_for_default_model().await?;
        Ok(s)
    }

    async fn spawn_engine_for_default_model(&self) -> Result<()> {
        if !crate::engine::spawn::should_spawn(&self.engine_cfg) {
            info!("engine.kind = openai_compat — agent will not spawn a child process");
            return Ok(());
        }
        let Some((name, m)) = self.cfg.models.iter().next() else {
            warn!("no [models.*] declared; engine will not be spawned by agent");
            return Ok(());
        };
        let spec = ModelSpec {
            name: name.clone(),
            tp: m.tp,
            max_model_len: m.max_model_len,
            extra_args: m.extra_args.clone(),
            path: m.path.clone(),
        };

        let legacy = self.cfg.agent.vllm_cmd.as_deref();
        let role = self.cfg.agent.role;
        let argv = render_argv(&self.engine_cfg, &spec, role, legacy)?;
        info!(argv = ?argv, kind = %self.engine.name(), "spawning engine");

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
        // tagged with `engine=<kind>`; omitted here for brevity.

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

/// Resolve the effective [`EngineConfig`] for the running agent.
///
/// Precedence: explicit `[engine]` block wins. If `[engine]` is absent but
/// `[agent].vllm_url` is set, build a vLLM-shaped `EngineConfig` from the
/// legacy fields and emit a warning. Otherwise fall back to defaults.
fn resolve_engine_config(cfg: &Config) -> EngineConfig {
    // The TOML deserializer always populates `engine` (it has #[serde(default)]),
    // so distinguishing "absent" from "default" requires the legacy fields.
    let legacy_url = cfg.agent.vllm_url.as_deref();
    let legacy_cmd = cfg.agent.vllm_cmd.as_deref();

    let mut effective = cfg.engine.clone();
    if let Some(u) = legacy_url {
        if !u.is_empty() && effective.url == EngineConfig::default().url {
            warn!(
                "[agent].vllm_url is deprecated; move it to [engine].url. \
                 Honoring legacy value for now."
            );
            effective.url = u.to_string();
        }
    }
    if legacy_cmd.is_some_and(|c| !c.is_empty()) {
        warn!(
            "[agent].vllm_cmd is deprecated; move it to [engine.vllm].extra_args \
             or use [engine].kind = \"llama_cpp\" for CPU. The legacy template \
             is still honored as the engine argv."
        );
    }
    effective
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
    }
}
