//! `cgn-infer` — Cognitora native inference engine.
//!
//! * `serve` — load a GGUF and serve the OpenAI HTTP/SSE surface with
//!   continuous batching. With `--role coordinator` the model's
//!   middle layers run on remote pipeline workers.
//! * `worker` — load only a layer slice and serve the
//!   `cognitora.v1.InferPipeline` gRPC activation stream.

use std::net::SocketAddr;
use std::path::PathBuf;

use cgn_core::{Error, Result};
use cgn_infer::engine::{Engine, EngineConfig};
use cgn_infer::model::LayerRange;
use cgn_infer::pipeline::{CoordinatorConfig, Encoding, TlsPaths};
use cgn_infer::runtime::Backend;
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "cgn-infer",
    version,
    about = "Cognitora native inference engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum Role {
    /// Single-node engine (no pipeline).
    #[default]
    Standalone,
    /// Pipeline head: local layers + remote workers.
    Coordinator,
}

/// mTLS flags shared by both roles. All three must be given together.
#[derive(Args, Debug, Clone)]
struct TlsArgs {
    /// CA bundle used to verify peers (enables mTLS when set).
    #[arg(long)]
    tls_ca: Option<PathBuf>,
    /// This process's certificate (PEM).
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// This process's private key (PEM).
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// Domain name expected on peer certificates.
    #[arg(long, default_value = "cognitora")]
    tls_domain: String,
}

impl TlsArgs {
    fn resolve(&self) -> Result<Option<TlsPaths>> {
        match (&self.tls_ca, &self.tls_cert, &self.tls_key) {
            (None, None, None) => Ok(None),
            (Some(ca), Some(cert), Some(key)) => Ok(Some(TlsPaths {
                ca: ca.clone(),
                cert: cert.clone(),
                key: key.clone(),
                domain: self.tls_domain.clone(),
            })),
            _ => Err(Error::Config(
                "--tls-ca, --tls-cert and --tls-key must be given together".into(),
            )),
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Load a GGUF model and serve the OpenAI-compatible HTTP API.
    Serve {
        /// Path to the GGUF model file.
        #[arg(long)]
        model: PathBuf,
        /// Listen host.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Listen port.
        #[arg(long, default_value_t = 8001)]
        port: u16,
        /// Context window (clamped to the model's training context).
        #[arg(long, default_value_t = 4096)]
        ctx: usize,
        /// CPU threads for the compute backend (0 = all cores).
        #[arg(long, default_value_t = 0)]
        threads: usize,
        /// Compute backend.
        #[arg(long, value_enum, default_value_t = Backend::Auto)]
        backend: Backend,
        /// Model id reported by /v1/models (defaults to the GGUF's
        /// embedded name).
        #[arg(long)]
        model_id: Option<String>,
        /// Max sequences decoded per batched step.
        #[arg(long, default_value_t = 8)]
        max_batch: usize,
        /// Prefill chunk size in tokens.
        #[arg(long, default_value_t = 512)]
        prefill_chunk: usize,
        /// KV pool budget in tokens shared by all resident sequences.
        #[arg(long, default_value_t = 65536)]
        kv_pool_tokens: usize,
        /// Engine role.
        #[arg(long, value_enum, default_value_t = Role::Standalone)]
        role: Role,
        /// Coordinator's local layer slice, `A:B` (must start at 0).
        /// Required with --role coordinator.
        #[arg(long)]
        layers: Option<String>,
        /// Worker gRPC endpoints in pipeline order, comma separated
        /// (e.g. `http://10.0.0.2:9101,http://10.0.0.3:9101`).
        #[arg(long, value_delimiter = ',')]
        workers: Vec<String>,
        /// Activation wire encoding for pipeline transport.
        #[arg(long, value_enum, default_value_t = Encoding::F16)]
        activation_encoding: Encoding,
        #[command(flatten)]
        tls: TlsArgs,
    },
    /// Load a layer slice and serve the pipeline activation stream.
    Worker {
        /// Path to the GGUF model file (same file as the coordinator).
        #[arg(long)]
        model: PathBuf,
        /// Layer slice to bind, `A:B` (half-open).
        #[arg(long)]
        layers: String,
        /// gRPC listen address.
        #[arg(long, default_value = "127.0.0.1:9101")]
        listen: String,
        /// CPU threads for the compute backend (0 = all cores).
        #[arg(long, default_value_t = 0)]
        threads: usize,
        /// Compute backend.
        #[arg(long, value_enum, default_value_t = Backend::Auto)]
        backend: Backend,
        /// Activation wire encoding for responses.
        #[arg(long, value_enum, default_value_t = Encoding::F16)]
        activation_encoding: Encoding,
        #[command(flatten)]
        tls: TlsArgs,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-infer");
    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            model,
            host,
            port,
            ctx,
            threads,
            backend,
            model_id,
            max_batch,
            prefill_chunk,
            kv_pool_tokens,
            role,
            layers,
            workers,
            activation_encoding,
            tls,
        } => {
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .map_err(|e| Error::Config(format!("listen address: {e}")))?;
            let pipeline = match role {
                Role::Standalone => {
                    if !workers.is_empty() || layers.is_some() {
                        return Err(Error::Config(
                            "--workers/--layers require --role coordinator".into(),
                        ));
                    }
                    None
                }
                Role::Coordinator => {
                    let layers = layers.ok_or_else(|| {
                        Error::Config("--role coordinator requires --layers A:B".into())
                    })?;
                    if workers.is_empty() {
                        return Err(Error::Config(
                            "--role coordinator requires at least one --workers endpoint".into(),
                        ));
                    }
                    Some(CoordinatorConfig {
                        layers: LayerRange::parse(&layers)?,
                        workers,
                        encoding: activation_encoding,
                        tls: tls.resolve()?,
                    })
                }
            };
            info!(model = %model.display(), %addr, ?backend, ctx, ?role, "cgn-infer starting");

            // Weight loading is CPU/IO heavy; keep it off the reactor.
            let cfg = EngineConfig {
                model_path: model,
                backend,
                ctx,
                threads,
                model_id,
                max_batch,
                prefill_chunk,
                kv_pool_tokens,
                pipeline,
            };
            let engine = tokio::task::spawn_blocking(move || Engine::load(&cfg))
                .await
                .map_err(|e| Error::Internal(format!("engine load task: {e}")))??;

            tokio::select! {
                r = cgn_infer::server::serve(engine, addr) => r,
                _ = tokio::signal::ctrl_c() => {
                    info!("cgn-infer shutting down");
                    Ok(())
                }
            }
        }
        Command::Worker {
            model,
            layers,
            listen,
            threads,
            backend,
            activation_encoding,
            tls,
        } => {
            if threads > 0 {
                std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
            }
            let addr: SocketAddr = listen
                .parse()
                .map_err(|e| Error::Config(format!("worker listen address: {e}")))?;
            let range = LayerRange::parse(&layers)?;
            info!(model = %model.display(), %addr, layers = %range, "cgn-infer worker starting");
            tokio::select! {
                r = cgn_infer::pipeline::run_worker(
                    model, range, addr, backend, activation_encoding, tls.resolve()?,
                ) => r,
                _ = tokio::signal::ctrl_c() => {
                    info!("cgn-infer worker shutting down");
                    Ok(())
                }
            }
        }
    }
}
