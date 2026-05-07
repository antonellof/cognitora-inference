//! `cgn-ctl install` — bare-metal, Kubernetes, and cloud installs.
//!
//! The single-node target is fully scripted: it writes a
//! `cognitora.toml` and `compose.yaml` into `--out-dir` (default
//! `./cognitora-single-node`) and, when `--apply` is set, runs
//! `docker compose up -d`. Three containers come up: etcd, the
//! router, and an agent shaped for the requested engine.
//!
//! The Kubernetes target shells out to `helm install`; cloud targets
//! locate `deploy/terraform/<provider>` and run
//! `terraform init && terraform apply`.

use std::path::{Path, PathBuf};

use cgn_core::{Error, Result};
use clap::{Args as ClapArgs, ValueEnum};
use tokio::process::Command;
use tracing::{info, warn};

mod single_node;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Where to install.
    #[arg(long, value_enum, default_value_t = Target::SingleNode)]
    pub target: Target,

    /// Model name (e.g. "llama3-8b"). Becomes both the `[models.*]`
    /// key in the generated config and the model id served by the
    /// router. Optional — without it the cluster comes up empty and
    /// you load models afterwards with `cgn-ctl model load`.
    #[arg(long)]
    pub model: Option<String>,

    /// HuggingFace repo or local path the engine should serve. Maps
    /// to `[models.<name>].hf_repo` for vLLM/SGLang or
    /// `[models.<name>].path` for llama.cpp. Defaults to the model
    /// name when omitted.
    #[arg(long)]
    pub hf_repo: Option<String>,

    /// Engine driver. Vllm by default; openai_compat lets you point
    /// the agent at an externally managed engine (e.g. Ollama,
    /// llama.cpp's own server).
    #[arg(long, value_enum, default_value_t = Engine::Vllm)]
    pub engine: Engine,

    /// Tensor parallelism for vLLM/SGLang. Ignored for `llama_cpp`
    /// and `openai_compat`.
    #[arg(long, default_value_t = 1)]
    pub tp: u32,

    /// Override the GHCR image tag used for router + agent.
    #[arg(long, default_value = "ghcr.io/antonellof/cognitora:v0.2.1")]
    pub image: String,

    /// Output directory for `cognitora.toml` and `compose.yaml`.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Actually run `docker compose up -d` after writing the files.
    /// Without this the command is a "render only" dry run, suitable
    /// for review or for piping into a different orchestrator.
    #[arg(long)]
    pub apply: bool,

    /// Override the chart path used for k8s installs.
    #[arg(long)]
    pub chart: Option<PathBuf>,

    /// Kubernetes namespace.
    #[arg(long, default_value = "cognitora")]
    pub namespace: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Target {
    SingleNode,
    Kubernetes,
    Aws,
    Gcp,
    Azure,
    Hetzner,
    Baremetal,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Engine {
    Vllm,
    Sglang,
    LlamaCpp,
    OpenaiCompat,
}

impl Engine {
    pub(crate) fn as_toml(self) -> &'static str {
        match self {
            Engine::Vllm => "vllm",
            Engine::Sglang => "sglang",
            Engine::LlamaCpp => "llama_cpp",
            Engine::OpenaiCompat => "openai_compat",
        }
    }
}

pub async fn run(args: Args) -> Result<()> {
    info!(?args.target, model = ?args.model, "install");
    match args.target {
        Target::SingleNode => single_node_install(args).await,
        Target::Kubernetes => kubernetes(args).await,
        Target::Aws | Target::Gcp | Target::Azure | Target::Hetzner | Target::Baremetal => {
            terraform(args).await
        }
    }
}

async fn single_node_install(args: Args) -> Result<()> {
    info!("preflight: docker?");
    let runner = preflight_runner()?;
    info!(runner = %runner, "container runtime ok");

    info!("preflight: NVIDIA driver?");
    if which::which("nvidia-smi").is_err() {
        warn!("nvidia-smi not found; the agent will run in CPU mode (vLLM will refuse to start)");
    }

    let dir = args
        .out_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("./cognitora-single-node"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("mkdir {dir:?}: {e}"))))?;

    let plan = single_node::Plan::from_args(&args);
    let toml = single_node::render_toml(&plan);
    let compose = single_node::render_compose(&plan);

    let toml_path = dir.join("cognitora.toml");
    let compose_path = dir.join("compose.yaml");
    write_atomic(&toml_path, &toml)?;
    write_atomic(&compose_path, &compose)?;

    info!(path = %toml_path.display(),    "wrote config");
    info!(path = %compose_path.display(), "wrote compose");

    if !args.apply {
        println!(
            "rendered single-node install in {}\n\
             review the files, then run:\n\
             \n  {} compose -f {} up -d\n",
            dir.display(),
            runner,
            compose_path.display(),
        );
        return Ok(());
    }

    info!("running `{} compose up -d`", runner);
    let status = Command::new(&runner)
        .arg("compose")
        .arg("-f")
        .arg(&compose_path)
        .arg("up")
        .arg("-d")
        .status()
        .await
        .map_err(|e| Error::Internal(format!("spawn {runner}: {e}")))?;
    if !status.success() {
        return Err(Error::Internal(format!(
            "{runner} compose up failed with {status}"
        )));
    }
    println!(
        "Cognitora single-node is up.\n  router HTTP: http://localhost:8080/v1\n  router admin: http://localhost:9091/metrics\n  etcd:        http://localhost:2379\n\nteardown: {} compose -f {} down",
        runner,
        compose_path.display(),
    );
    Ok(())
}

fn preflight_runner() -> Result<String> {
    if which::which("docker").is_ok() {
        return Ok("docker".into());
    }
    if which::which("podman").is_ok() {
        return Ok("podman".into());
    }
    Err(Error::Unavailable(
        "neither docker nor podman found in PATH".into(),
    ))
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("write {tmp:?}: {e}"))))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("rename {tmp:?} -> {path:?}: {e}"),
        ))
    })?;
    Ok(())
}

async fn kubernetes(args: Args) -> Result<()> {
    info!("preflight: helm");
    let v = cgn_helm::version().await?;
    info!(helm = %v, "helm available");

    let chart = args
        .chart
        .unwrap_or_else(|| PathBuf::from("deploy/kubernetes/helm/cognitora"));
    let install = cgn_helm::Install {
        release: "cognitora".into(),
        chart,
        namespace: args.namespace,
        create_namespace: true,
        values: vec![],
        set: vec![],
        wait: true,
        timeout: Some("10m".into()),
    };
    let out = install.run().await?;
    println!("{out}");
    Ok(())
}

async fn terraform(args: Args) -> Result<()> {
    let module = match args.target {
        Target::Aws => "deploy/terraform/aws",
        Target::Gcp => "deploy/terraform/gcp",
        Target::Azure => "deploy/terraform/azure",
        Target::Hetzner => "deploy/terraform/hetzner",
        Target::Baremetal => "deploy/terraform/baremetal",
        _ => unreachable!("non-terraform target reached terraform()"),
    };
    let module = PathBuf::from(module);
    if !module.exists() {
        return Err(Error::NotFound(format!(
            "terraform module not found: {}",
            module.display()
        )));
    }
    if which::which("terraform").is_err() {
        return Err(Error::Unavailable(
            "terraform not in PATH; install hashicorp terraform first".into(),
        ));
    }

    if !args.apply {
        println!(
            "{} ready. To apply, run:\n  terraform -chdir={} init\n  terraform -chdir={} apply",
            module.display(),
            module.display(),
            module.display(),
        );
        return Ok(());
    }

    info!(module = %module.display(), "terraform init");
    let status = Command::new("terraform")
        .arg("-chdir")
        .arg(&module)
        .arg("init")
        .status()
        .await
        .map_err(|e| Error::Internal(format!("terraform init: {e}")))?;
    if !status.success() {
        return Err(Error::Internal(format!("terraform init failed: {status}")));
    }
    info!(module = %module.display(), "terraform apply");
    let status = Command::new("terraform")
        .arg("-chdir")
        .arg(&module)
        .arg("apply")
        .arg("-auto-approve")
        .status()
        .await
        .map_err(|e| Error::Internal(format!("terraform apply: {e}")))?;
    if !status.success() {
        return Err(Error::Internal(format!("terraform apply failed: {status}")));
    }
    Ok(())
}
