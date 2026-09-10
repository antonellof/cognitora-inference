//! Agent gRPC server: implements `cognitora.v1.Agent`.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use cgn_core::{Error, Result};
use cgn_proto::v1::{
    agent_server::{Agent, AgentServer},
    embed_response::Vector as PVector,
    AgentGenerateRequest, EmbedRequest as PEmbedRequest, EmbedResponse as PEmbedResponse,
    KvHandoffSpec, ModelRef, ModelSpec, NodeHealth, Status as PStatus, Token,
};
use futures::Stream;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Server, Request, Response, Status, Streaming};
use tracing::info;

use crate::engine::{EmbedReq, GenerateReq};
use crate::supervisor::Supervisor;

pub async fn serve(supervisor: Arc<Supervisor>, addr: SocketAddr) -> Result<()> {
    info!(%addr, "agent grpc listening");

    // Long-running `Generate` streams (MLX first download/compile, huge
    // prompts) can exceed a short tonic server timeout; 2h matches the
    // reqwest engine client budget in `openai_http`.
    let mut builder = Server::builder().timeout(std::time::Duration::from_secs(7200));

    if supervisor.cfg.security.require_mtls {
        let (Some(ca), Some(cert), Some(key)) = (
            supervisor.cfg.security.ca_file.as_ref(),
            supervisor.cfg.security.cert_file.as_ref(),
            supervisor.cfg.security.key_file.as_ref(),
        ) else {
            return Err(Error::Config(
                "require_mtls=true but cert/key/ca not set".into(),
            ));
        };
        let tls = cgn_tls::server_tls(ca, cert, key)?;
        builder = builder
            .tls_config(tls)
            .map_err(|e| Error::Tls(format!("server tls: {e}")))?;
    }

    let svc = AgentSvc {
        supervisor: supervisor.clone(),
    };
    builder
        .add_service(AgentServer::new(svc))
        .serve(addr)
        .await
        .map_err(|e| Error::Internal(format!("agent grpc serve: {e}")))
}

struct AgentSvc {
    supervisor: Arc<Supervisor>,
}

type GenStream = Pin<Box<dyn Stream<Item = Result<Token, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Agent for AgentSvc {
    type GenerateStream = GenStream;

    async fn generate(
        &self,
        req: Request<Streaming<AgentGenerateRequest>>,
    ) -> Result<Response<Self::GenerateStream>, Status> {
        let mut inbound = req.into_inner();
        let first = inbound
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("empty agent stream"))??;

        let (tx, rx) = mpsc::channel::<Result<Token, Status>>(64);
        let engine = self.supervisor.engine.clone();
        let supervisor = self.supervisor.clone();
        tokio::spawn(async move {
            let messages: Vec<crate::engine::ChatMessage> = first
                .messages
                .iter()
                .map(|m| crate::engine::ChatMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                    content_json: m.content_json.clone(),
                    tool_calls_json: m.tool_calls_json.clone(),
                    tool_call_id: m.tool_call_id.clone(),
                })
                .collect();
            // Legacy plain-prompt fallback used only if the caller
            // somehow gave us no messages (e.g. raw `/v1/completions`).
            let prompt = if messages.is_empty() {
                first
                    .messages
                    .iter()
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                String::new()
            };
            let p = first.params.unwrap_or_default();
            let confirmed_digests = first.digests.clone();
            let req = GenerateReq {
                id: first.id,
                model: first.model,
                messages,
                prompt,
                max_tokens: if p.max_tokens == 0 { 256 } else { p.max_tokens },
                temperature: p.temperature,
                top_p: p.top_p,
                stop: p.stop,
                stream: true,
                extensions_json: first.extensions_json,
            };
            let (e_tx, mut e_rx) = mpsc::channel::<Token>(64);
            let gen = engine.generate(req, e_tx);
            let pump = async {
                while let Some(t) = e_rx.recv().await {
                    if tx.send(Ok(t)).await.is_err() {
                        break;
                    }
                }
            };
            let (_, gen_res) = tokio::join!(pump, gen);
            match gen_res {
                Ok(()) => {
                    // Generation completed → the engine now holds the KV
                    // for this prompt's prefix. Queue the router-supplied
                    // digests as *confirmed* claims; the health loop
                    // publishes them to etcd under the heartbeat lease.
                    if !confirmed_digests.is_empty() {
                        supervisor.kv_confirmed.lock().extend(confirmed_digests);
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(Status::from(e))).await;
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn embed(&self, req: Request<PEmbedRequest>) -> Result<Response<PEmbedResponse>, Status> {
        let body = req.into_inner();
        if body.inputs.is_empty() {
            return Err(Status::invalid_argument("inputs must be non-empty"));
        }
        let model = body.model.clone();
        let er = EmbedReq {
            model: body.model,
            inputs: body.inputs,
        };
        match self.supervisor.engine.embed(er).await {
            Ok(out) => {
                let embeddings = out
                    .embeddings
                    .into_iter()
                    .map(|values| PVector { values })
                    .collect();
                Ok(Response::new(PEmbedResponse {
                    embeddings,
                    tokens: out.prompt_tokens,
                    model,
                }))
            }
            Err(cgn_core::Error::Unavailable(msg)) => Err(Status::unavailable(msg)),
            Err(e) => Err(Status::internal(format!("engine embed: {e}"))),
        }
    }

    async fn load_model(&self, req: Request<ModelSpec>) -> Result<Response<PStatus>, Status> {
        let s = req.into_inner();
        let name = s
            .r#ref
            .as_ref()
            .map(|r| r.name.as_str())
            .unwrap_or("")
            .to_string();
        info!(model=%name, "load_model");
        // Real impl would orchestrate engine reload; for now we acknowledge.
        Ok(Response::new(PStatus {
            code: 0,
            message: "ok".into(),
        }))
    }

    async fn unload_model(&self, _req: Request<ModelRef>) -> Result<Response<PStatus>, Status> {
        Ok(Response::new(PStatus {
            code: 0,
            message: "ok".into(),
        }))
    }

    async fn kv_handoff(&self, req: Request<KvHandoffSpec>) -> Result<Response<PStatus>, Status> {
        use cgn_proto::v1::kv_handoff_spec::Direction;
        use cgn_proto::v1::{kv_client::KvClient, PullSpec, PushSpec};

        let spec = req.into_inner();
        if spec.prefix_hash.len() != 32 {
            return Err(Status::invalid_argument("prefix_hash must be 32 bytes"));
        }

        // Bridge to the host-local cgn-kvcached daemon, which owns the
        // tiered store and drives the QUIC transfer with the peer.
        let listen = &self.supervisor.cfg.kv.listen;
        let local = listen.replace("0.0.0.0", "127.0.0.1");
        let uri = format!("http://{local}");
        let mut kv = KvClient::connect(uri)
            .await
            .map_err(|e| Status::unavailable(format!("kvcached connect: {e}")))?;

        let status = match spec.direction() {
            Direction::Push => kv
                .push(PushSpec {
                    prefix_hash: spec.prefix_hash,
                    target_node_id: spec.peer_node_id,
                    target_endpoint: spec.peer_endpoint,
                    use_rdma: spec.use_rdma,
                })
                .await?
                .into_inner(),
            Direction::Pull => kv
                .pull(PullSpec {
                    prefix_hash: spec.prefix_hash,
                    source_node_id: spec.peer_node_id,
                    source_endpoint: spec.peer_endpoint,
                    use_rdma: spec.use_rdma,
                })
                .await?
                .into_inner(),
            Direction::Unspecified => {
                return Err(Status::invalid_argument("direction must be PUSH or PULL"))
            }
        };
        Ok(Response::new(status))
    }

    async fn health(&self, _req: Request<()>) -> Result<Response<NodeHealth>, Status> {
        let ready = self.supervisor.engine.ready().await;
        let stats = crate::telemetry::scrape(
            self.supervisor.engine.name(),
            &self.supervisor.engine_cfg.url,
        )
        .await
        .unwrap_or_default();
        let gpu = crate::health::read_nvml_blocking().unwrap_or_default();
        Ok(Response::new(NodeHealth {
            node_id: self.supervisor.cfg.agent.node_id.clone(),
            ready,
            queue_depth: stats.queue_depth,
            max_queue: 1024,
            free_kv_blocks: stats.free_blocks,
            gpu_util_pct: gpu.util_pct,
            gpu_mem_used_pct: gpu.mem_used_pct,
            gpu_temp_c: gpu.temp_c,
            rack_watts: gpu.power_watts,
            rack_watt_limit: 0.0,
            last_seen_unix_ms: chrono::Utc::now().timestamp_millis() as u64,
            loaded_models: self.supervisor.cfg.models.keys().cloned().collect(),
            role: crate::health::role_to_int(&self.supervisor.cfg.agent.role),
        }))
    }

    async fn drain(&self, _req: Request<()>) -> Result<Response<PStatus>, Status> {
        self.supervisor.shutdown().await;
        Ok(Response::new(PStatus {
            code: 0,
            message: "drained".into(),
        }))
    }
}
