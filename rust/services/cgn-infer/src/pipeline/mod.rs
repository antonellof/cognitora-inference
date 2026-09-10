//! Distributed layer-pipeline inference (phase 4).
//!
//! The model is split by contiguous layer ranges. The **coordinator**
//! binds the token embedding, the first slice of layers, and the LM
//! head; each **worker** binds one middle/tail slice. All processes
//! mmap the same GGUF file, so a worker only pays (page-cache) memory
//! for the tensors it actually binds.
//!
//! Per forward step the coordinator embeds tokens, runs its local
//! layers, then streams the hidden states through each worker in
//! pipeline order over the `cognitora.v1.InferPipeline` bidi gRPC
//! stream (f16 activations by default, optional int8), and finally
//! applies the LM head locally. Transport uses mTLS when certificate
//! paths are configured, consistent with the rest of the platform.
//!
//! Concurrency: the pipeline carries one activation stream at a time
//! ([`PipelinedModel::max_batch`] == 1), so the scheduler serves
//! sequences one by one. Chunked prefill still applies. Only the
//! architectures covered by [`crate::model::QLlama`] (`llama`,
//! `qwen2`) can run in pipeline mode.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use candle_core::{Device, Tensor};
use cgn_core::{Error, Result};
use cgn_proto::v1::infer_pipeline_client::InferPipelineClient;
use cgn_proto::v1::infer_pipeline_server::{InferPipeline, InferPipelineServer};
use cgn_proto::v1::{
    ActivationChunk, ActivationEncoding, ResetSequenceRequest, ResetSequenceResponse, WorkerInfo,
    WorkerInfoRequest,
};
use futures::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::model::{GgufModel, LayerRange, ModelParts, QLlama, SeqKv};
use crate::runtime::BatchModel;

// ---------------------------------------------------------------------------
// Activation codecs
// ---------------------------------------------------------------------------

/// Wire encoding for hidden states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Encoding {
    /// 2 bytes/value, lossless enough for f32 activations.
    #[default]
    F16,
    /// 1 byte/value + per-tensor scale; halves bandwidth again at
    /// some accuracy cost.
    Int8,
}

/// Serialize a `(1, t, hidden)` f32 tensor into wire bytes.
pub fn encode_activation(t: &Tensor, enc: Encoding) -> Result<(Vec<u8>, f32)> {
    let values: Vec<f32> = t
        .flatten_all()
        .and_then(|t| t.to_dtype(candle_core::DType::F32))
        .and_then(|t| t.to_vec1::<f32>())
        .map_err(|e| Error::Internal(format!("activation readback: {e}")))?;
    match enc {
        Encoding::F16 => {
            let mut out = Vec::with_capacity(values.len() * 2);
            for v in values {
                out.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
            }
            Ok((out, 0.0))
        }
        Encoding::Int8 => {
            let max = values.iter().fold(0f32, |m, v| m.max(v.abs()));
            let scale = if max > 0.0 { max / 127.0 } else { 1.0 };
            let out = values
                .iter()
                .map(|v| (v / scale).round().clamp(-127.0, 127.0) as i8 as u8)
                .collect();
            Ok((out, scale))
        }
    }
}

/// Deserialize wire bytes back into a `(1, seq_len, hidden)` f32 tensor.
pub fn decode_activation(
    data: &[u8],
    encoding: ActivationEncoding,
    int8_scale: f32,
    seq_len: usize,
    hidden: usize,
    device: &Device,
) -> Result<Tensor> {
    let expect = seq_len * hidden;
    let values: Vec<f32> = match encoding {
        ActivationEncoding::F16 => {
            if data.len() != expect * 2 {
                return Err(Error::InvalidArgument(format!(
                    "f16 activation payload is {} bytes, expected {}",
                    data.len(),
                    expect * 2
                )));
            }
            data.chunks_exact(2)
                .map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32())
                .collect()
        }
        ActivationEncoding::Int8 => {
            if data.len() != expect {
                return Err(Error::InvalidArgument(format!(
                    "int8 activation payload is {} bytes, expected {expect}",
                    data.len()
                )));
            }
            data.iter().map(|&b| b as i8 as f32 * int8_scale).collect()
        }
        ActivationEncoding::Unspecified => {
            return Err(Error::InvalidArgument(
                "unspecified activation encoding".into(),
            ))
        }
    };
    Tensor::from_vec(values, (1, seq_len, hidden), device)
        .map_err(|e| Error::Internal(format!("activation tensor: {e}")))
}

fn wire_encoding(enc: Encoding) -> ActivationEncoding {
    match enc {
        Encoding::F16 => ActivationEncoding::F16,
        Encoding::Int8 => ActivationEncoding::Int8,
    }
}

// ---------------------------------------------------------------------------
// TLS plumbing
// ---------------------------------------------------------------------------

/// mTLS material shared by coordinator and worker CLIs.
#[derive(Debug, Clone)]
pub struct TlsPaths {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// SNI / certificate domain expected on peers.
    pub domain: String,
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

struct WorkerState {
    model: QLlama,
    kvs: HashMap<u64, SeqKv>,
    model_name: String,
}

/// Worker-side gRPC service: hidden states in, hidden states out.
pub struct WorkerService {
    state: Arc<Mutex<WorkerState>>,
    enc_out: Encoding,
}

impl WorkerService {
    fn step(
        state: &Mutex<WorkerState>,
        chunk: &ActivationChunk,
        enc_out: Encoding,
    ) -> Result<ActivationChunk> {
        let mut st = state.lock().expect("worker state poisoned");
        let device = st.model.device().clone();
        let hidden = decode_activation(
            &chunk.data,
            chunk
                .encoding
                .try_into()
                .unwrap_or(ActivationEncoding::Unspecified),
            chunk.int8_scale,
            chunk.seq_len as usize,
            chunk.hidden as usize,
            &device,
        )?;
        let kv = match st.kvs.entry(chunk.seq_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(_) => {
                let fresh = st.model.new_kv();
                st.kvs.entry(chunk.seq_id).or_insert(fresh)
            }
        };
        if chunk.index_pos == 0 {
            kv.clear();
        }
        if kv.len() != chunk.index_pos as usize {
            return Err(Error::InvalidArgument(format!(
                "seq {}: worker KV holds {} positions but coordinator sent index_pos {}",
                chunk.seq_id,
                kv.len(),
                chunk.index_pos
            )));
        }
        // Split borrows: pull the kv out, run, put back.
        let mut kv_owned = std::mem::take(kv);
        let result = st
            .model
            .forward_hidden(&hidden, chunk.index_pos as usize, &mut kv_owned);
        st.kvs.insert(chunk.seq_id, kv_owned);
        let out = result?;

        let (data, scale) = encode_activation(&out, enc_out)?;
        Ok(ActivationChunk {
            request_id: chunk.request_id,
            seq_id: chunk.seq_id,
            index_pos: chunk.index_pos,
            seq_len: chunk.seq_len,
            hidden: chunk.hidden,
            encoding: wire_encoding(enc_out) as i32,
            data,
            int8_scale: scale,
            error: String::new(),
        })
    }
}

#[tonic::async_trait]
impl InferPipeline for WorkerService {
    type ForwardStream =
        Pin<Box<dyn Stream<Item = std::result::Result<ActivationChunk, Status>> + Send>>;

    async fn forward(
        &self,
        request: Request<Streaming<ActivationChunk>>,
    ) -> std::result::Result<Response<Self::ForwardStream>, Status> {
        let mut inbound = request.into_inner();
        let state = self.state.clone();
        let enc_out = self.enc_out;
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            while let Some(msg) = inbound.next().await {
                let chunk = match msg {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(error = %e, "activation stream receive failed");
                        break;
                    }
                };
                let state2 = state.clone();
                // Forward passes are CPU-heavy; keep them off the reactor.
                let reply = tokio::task::spawn_blocking(move || {
                    WorkerService::step(&state2, &chunk, enc_out).unwrap_or_else(|e| {
                        ActivationChunk {
                            request_id: chunk.request_id,
                            seq_id: chunk.seq_id,
                            error: e.to_string(),
                            ..Default::default()
                        }
                    })
                })
                .await;
                let reply = match reply {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "worker step panicked");
                        break;
                    }
                };
                if tx.send(Ok(reply)).await.is_err() {
                    break; // coordinator went away
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn info(
        &self,
        _request: Request<WorkerInfoRequest>,
    ) -> std::result::Result<Response<WorkerInfo>, Status> {
        let st = self.state.lock().expect("worker state poisoned");
        Ok(Response::new(WorkerInfo {
            model: st.model_name.clone(),
            architecture: st.model.cfg.architecture.clone(),
            layer_start: st.model.range.start as u32,
            layer_end: st.model.range.end as u32,
            total_layers: st.model.cfg.n_layers as u32,
            ready: true,
        }))
    }

    async fn reset_sequence(
        &self,
        request: Request<ResetSequenceRequest>,
    ) -> std::result::Result<Response<ResetSequenceResponse>, Status> {
        let seq = request.into_inner().seq_id;
        self.state
            .lock()
            .expect("worker state poisoned")
            .kvs
            .remove(&seq);
        Ok(Response::new(ResetSequenceResponse {}))
    }
}

/// Load a layer slice and serve the `InferPipeline` gRPC surface.
pub async fn run_worker(
    model_path: PathBuf,
    layers: LayerRange,
    listen: SocketAddr,
    backend: crate::runtime::Backend,
    encoding: Encoding,
    tls: Option<TlsPaths>,
) -> Result<()> {
    let device = crate::runtime::pick_device(backend)?;
    let state = tokio::task::spawn_blocking(move || -> Result<WorkerState> {
        let gguf = GgufModel::open(&model_path)?;
        let mut reader = gguf.cursor();
        let model = QLlama::load(
            &gguf.content,
            &mut reader,
            &device,
            layers,
            ModelParts::middle(),
        )?;
        Ok(WorkerState {
            model,
            kvs: HashMap::new(),
            model_name: gguf.metadata.name.clone(),
        })
    })
    .await
    .map_err(|e| Error::Internal(format!("worker load task: {e}")))??;

    info!(%listen, layers = %layers, "cgn-infer worker ready");
    let svc = WorkerService {
        state: Arc::new(Mutex::new(state)),
        enc_out: encoding,
    };
    let mut builder = Server::builder();
    if let Some(t) = &tls {
        let cfg = cgn_tls::server_tls(&t.ca, &t.cert, &t.key)?;
        builder = builder
            .tls_config(cfg)
            .map_err(|e| Error::Tls(format!("worker tls: {e}")))?;
    }
    builder
        .add_service(InferPipelineServer::new(svc))
        .serve(listen)
        .await
        .map_err(|e| Error::Internal(format!("worker grpc server: {e}")))
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Coordinator-side pipeline settings (from the CLI / agent argv).
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Layer slice run locally by the coordinator; must start at 0.
    pub layers: LayerRange,
    /// Worker endpoints in pipeline order (`http://` or `https://`).
    pub workers: Vec<String>,
    pub encoding: Encoding,
    pub tls: Option<TlsPaths>,
}

/// One connected worker: a live bidi activation stream.
struct WorkerLink {
    endpoint: String,
    tx: mpsc::Sender<ActivationChunk>,
    rx: Streaming<ActivationChunk>,
    client: InferPipelineClient<Channel>,
    range: LayerRange,
}

/// [`BatchModel`] whose middle layers run on remote workers.
///
/// Sequential by construction (`max_batch() == 1`): one activation
/// stream is in flight at a time. The scheduler still provides
/// queueing, chunked prefill, and KV accounting on top.
pub struct PipelinedModel {
    head: QLlama,
    kvs: HashMap<u64, SeqKv>,
    workers: Vec<WorkerLink>,
    handle: tokio::runtime::Handle,
    encoding: Encoding,
    max_seq_len: usize,
    next_request_id: u64,
}

impl PipelinedModel {
    /// Load the local stage, connect to every worker, and validate
    /// that the slices tile the model exactly. Must be called from
    /// within a tokio runtime context (e.g. `spawn_blocking`).
    pub fn connect(
        gguf: &GgufModel,
        device: &Device,
        ctx: usize,
        cfg: &CoordinatorConfig,
    ) -> Result<Self> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            Error::Internal("PipelinedModel::connect requires a tokio runtime context".into())
        })?;
        if cfg.layers.start != 0 {
            return Err(Error::Config(format!(
                "coordinator layer range must start at 0, got {}",
                cfg.layers
            )));
        }
        let mut reader = gguf.cursor();
        let head = QLlama::load(
            &gguf.content,
            &mut reader,
            device,
            cfg.layers,
            ModelParts::full(),
        )?;
        let max_seq_len = ctx.min(head.cfg.context_length).max(1);

        let mut workers = Vec::with_capacity(cfg.workers.len());
        for endpoint in &cfg.workers {
            let link = handle.block_on(Self::connect_worker(endpoint, cfg.tls.as_ref()))?;
            workers.push(link);
        }

        // The local slice plus worker slices must tile [0, n_layers).
        let mut ranges = vec![cfg.layers];
        ranges.extend(workers.iter().map(|w| w.range));
        crate::model::validate_coverage(head.cfg.n_layers, &ranges)?;
        info!(
            local = %cfg.layers,
            workers = workers.len(),
            total_layers = head.cfg.n_layers,
            "pipeline assembled"
        );
        Ok(Self {
            head,
            kvs: HashMap::new(),
            workers,
            handle,
            encoding: cfg.encoding,
            max_seq_len,
            next_request_id: 1,
        })
    }

    async fn connect_worker(endpoint: &str, tls: Option<&TlsPaths>) -> Result<WorkerLink> {
        let mut ep = Channel::from_shared(endpoint.to_string())
            .map_err(|e| Error::Config(format!("worker endpoint {endpoint}: {e}")))?;
        if let Some(t) = tls {
            let cfg = cgn_tls::client_tls(&t.ca, &t.cert, &t.key, t.domain.clone())?;
            ep = ep
                .tls_config(cfg)
                .map_err(|e| Error::Tls(format!("worker client tls: {e}")))?;
        }
        let channel = ep
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("connect worker {endpoint}: {e}")))?;
        let mut client = InferPipelineClient::new(channel);

        let info = client
            .info(WorkerInfoRequest {})
            .await
            .map_err(|e| Error::Unavailable(format!("worker {endpoint} info: {e}")))?
            .into_inner();
        let range = LayerRange::new(info.layer_start as usize, info.layer_end as usize)?;

        let (tx, rx_out) = mpsc::channel::<ActivationChunk>(4);
        let rx = client
            .forward(ReceiverStream::new(rx_out))
            .await
            .map_err(|e| Error::Unavailable(format!("worker {endpoint} forward: {e}")))?
            .into_inner();
        info!(endpoint, layers = %range, "worker connected");
        Ok(WorkerLink {
            endpoint: endpoint.to_string(),
            tx,
            rx,
            client,
            range,
        })
    }

    /// Push `hidden` through every worker in order, awaiting each
    /// response (the stream carries one step at a time).
    fn forward_remote(
        &mut self,
        hidden: Tensor,
        seq: u64,
        index_pos: usize,
        seq_len: usize,
    ) -> Result<Tensor> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let hidden_dim = self.head.cfg.embedding_length;
        let device = self.head.device().clone();
        let encoding = self.encoding;
        let mut current = hidden;
        for w in &mut self.workers {
            let (data, scale) = encode_activation(&current, encoding)?;
            let chunk = ActivationChunk {
                request_id,
                seq_id: seq,
                index_pos: index_pos as u32,
                seq_len: seq_len as u32,
                hidden: hidden_dim as u32,
                encoding: wire_encoding(encoding) as i32,
                data,
                int8_scale: scale,
                error: String::new(),
            };
            let endpoint = w.endpoint.clone();
            let reply = self.handle.block_on(async {
                w.tx.send(chunk)
                    .await
                    .map_err(|_| Error::Unavailable(format!("worker {endpoint} stream closed")))?;
                w.rx.message()
                    .await
                    .map_err(|e| Error::Unavailable(format!("worker {endpoint} recv: {e}")))?
                    .ok_or_else(|| {
                        Error::Unavailable(format!("worker {endpoint} closed the stream"))
                    })
            })?;
            if !reply.error.is_empty() {
                return Err(Error::Internal(format!(
                    "worker {}: {}",
                    w.endpoint, reply.error
                )));
            }
            current = decode_activation(
                &reply.data,
                reply
                    .encoding
                    .try_into()
                    .unwrap_or(ActivationEncoding::Unspecified),
                reply.int8_scale,
                reply.seq_len as usize,
                reply.hidden as usize,
                &device,
            )?;
        }
        Ok(current)
    }

    fn reset_remote(&mut self, seq: u64) {
        for w in &mut self.workers {
            let mut client = w.client.clone();
            let endpoint = w.endpoint.clone();
            let res = self.handle.block_on(async move {
                client
                    .reset_sequence(ResetSequenceRequest { seq_id: seq })
                    .await
            });
            if let Err(e) = res {
                warn!(endpoint, error = %e, "reset_sequence failed");
            }
        }
    }
}

impl BatchModel for PipelinedModel {
    fn max_batch(&self) -> usize {
        1
    }

    fn max_seq_len(&self) -> usize {
        self.max_seq_len
    }

    fn prefill(
        &mut self,
        seq: u64,
        tokens: &[u32],
        start_pos: usize,
        want_logits: bool,
    ) -> Result<Option<Vec<f32>>> {
        if start_pos == 0 {
            self.kvs.insert(seq, self.head.new_kv());
        }
        let mut kv = self
            .kvs
            .remove(&seq)
            .ok_or_else(|| Error::Internal(format!("prefill of unknown seq {seq}")))?;
        let result = (|| -> Result<Option<Vec<f32>>> {
            if kv.len() != start_pos {
                return Err(Error::Internal(format!(
                    "prefill position mismatch for seq {seq}: kv {} vs start_pos {start_pos}",
                    kv.len()
                )));
            }
            let hidden = self.head.embed_tokens(tokens)?;
            let hidden = self.head.forward_hidden(&hidden, start_pos, &mut kv)?;
            let hidden = self.forward_remote(hidden, seq, start_pos, tokens.len())?;
            if !want_logits {
                return Ok(None);
            }
            let logits = self.head.output(&hidden)?;
            logits
                .squeeze(0)
                .and_then(|t| t.to_dtype(candle_core::DType::F32))
                .and_then(|t| t.to_vec1::<f32>())
                .map(Some)
                .map_err(|e| Error::Internal(format!("logits readback: {e}")))
        })();
        self.kvs.insert(seq, kv);
        result
    }

    fn decode(&mut self, batch: &[(u64, u32)]) -> Result<Vec<Vec<f32>>> {
        match batch {
            [] => Ok(vec![]),
            [(seq, token)] => {
                let seq = *seq;
                let mut kv = self
                    .kvs
                    .remove(&seq)
                    .ok_or_else(|| Error::Internal(format!("decode of unknown seq {seq}")))?;
                let result = (|| -> Result<Vec<f32>> {
                    let pos = kv.len();
                    let hidden = self.head.embed_tokens(&[*token])?;
                    let hidden = self.head.forward_hidden(&hidden, pos, &mut kv)?;
                    let hidden = self.forward_remote(hidden, seq, pos, 1)?;
                    let logits = self.head.output(&hidden)?;
                    logits
                        .squeeze(0)
                        .and_then(|t| t.to_dtype(candle_core::DType::F32))
                        .and_then(|t| t.to_vec1::<f32>())
                        .map_err(|e| Error::Internal(format!("logits readback: {e}")))
                })();
                self.kvs.insert(seq, kv);
                Ok(vec![result?])
            }
            _ => Err(Error::Internal(
                "pipeline runtime cannot decode a batch larger than 1".into(),
            )),
        }
    }

    fn drop_seq(&mut self, seq: u64) {
        if self.kvs.remove(&seq).is_some() {
            self.reset_remote(seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_roundtrip() {
        let dev = Device::Cpu;
        let t = Tensor::from_vec(
            vec![0.5f32, -1.25, 3.0, 0.0, 100.0, -0.001],
            (1, 2, 3),
            &dev,
        )
        .unwrap();
        let (bytes, scale) = encode_activation(&t, Encoding::F16).unwrap();
        assert_eq!(bytes.len(), 12);
        let back = decode_activation(&bytes, ActivationEncoding::F16, scale, 2, 3, &dev).unwrap();
        let orig: Vec<f32> = t.flatten_all().unwrap().to_vec1().unwrap();
        let round: Vec<f32> = back.flatten_all().unwrap().to_vec1().unwrap();
        for (a, b) in orig.iter().zip(&round) {
            assert!((a - b).abs() <= a.abs() * 1e-3 + 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn int8_roundtrip_is_approximate() {
        let dev = Device::Cpu;
        let t = Tensor::from_vec(vec![0.5f32, -1.0, 1.0, 0.25], (1, 1, 4), &dev).unwrap();
        let (bytes, scale) = encode_activation(&t, Encoding::Int8).unwrap();
        assert_eq!(bytes.len(), 4);
        let back = decode_activation(&bytes, ActivationEncoding::Int8, scale, 1, 4, &dev).unwrap();
        let round: Vec<f32> = back.flatten_all().unwrap().to_vec1().unwrap();
        let orig = [0.5f32, -1.0, 1.0, 0.25];
        for (a, b) in orig.iter().zip(&round) {
            assert!((a - b).abs() <= 1.0 / 127.0 + 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn int8_all_zero_tensor() {
        let dev = Device::Cpu;
        let t = Tensor::zeros((1, 1, 4), candle_core::DType::F32, &dev).unwrap();
        let (bytes, scale) = encode_activation(&t, Encoding::Int8).unwrap();
        let back = decode_activation(&bytes, ActivationEncoding::Int8, scale, 1, 4, &dev).unwrap();
        let round: Vec<f32> = back.flatten_all().unwrap().to_vec1().unwrap();
        assert!(round.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn decode_rejects_wrong_length() {
        let dev = Device::Cpu;
        assert!(decode_activation(&[0u8; 3], ActivationEncoding::F16, 0.0, 1, 2, &dev).is_err());
        assert!(decode_activation(&[0u8; 3], ActivationEncoding::Int8, 1.0, 1, 2, &dev).is_err());
        assert!(
            decode_activation(&[0u8; 4], ActivationEncoding::Unspecified, 0.0, 1, 2, &dev).is_err()
        );
    }
}
