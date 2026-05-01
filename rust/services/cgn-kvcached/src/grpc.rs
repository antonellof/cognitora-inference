//! KV daemon gRPC server (TCP + UDS).

use std::net::SocketAddr;
use std::sync::Arc;

use cgn_core::{config::Config, Error, Result};
use cgn_proto::v1::{
    block_info::Tier as PTier,
    kv_server::{Kv, KvServer},
    BlockInfo, BlockInfoList, Hash, HashList, PullSpec, PushSpec, Status as PStatus,
    StatsRequest, StatsResponse,
};
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;

use crate::tiers::Store;

pub async fn serve(store: Arc<Store>, addr: SocketAddr, cfg: &Config) -> Result<()> {
    info!(%addr, "kvcached grpc listening");
    let mut builder = Server::builder().timeout(std::time::Duration::from_secs(60));

    if cfg.security.require_mtls {
        let (Some(ca), Some(cert), Some(key)) = (
            cfg.security.ca_file.as_ref(),
            cfg.security.cert_file.as_ref(),
            cfg.security.key_file.as_ref(),
        ) else {
            return Err(Error::Config("require_mtls=true but cert/key/ca not set".into()));
        };
        let tls = cgn_tls::server_tls(ca, cert, key)?;
        builder = builder.tls_config(tls)
            .map_err(|e| Error::Tls(format!("server tls: {e}")))?;
    }

    let svc = KvSvc { store };
    builder
        .add_service(KvServer::new(svc))
        .serve(addr).await
        .map_err(|e| Error::Internal(format!("kv grpc serve: {e}")))
}

struct KvSvc { store: Arc<Store> }

#[tonic::async_trait]
impl Kv for KvSvc {
    async fn lookup(&self, req: Request<Hash>) -> Result<Response<BlockInfo>, Status> {
        let h = req.into_inner();
        let digest = digest_from_bytes(&h.value)?;
        let addr = cgn_kv::BlockAddress { digest, layer: 0 };
        let info = match self.store.lookup(&addr) {
            Some(handle) => BlockInfo {
                prefix_hash: digest.to_vec(),
                tier: tier_to_proto(handle.meta.tier).into(),
                size_bytes: handle.meta.bytes,
                owner_node: String::new(),
                last_access_unix_ms: handle.meta.last_seen_unix * 1000,
            },
            None => BlockInfo {
                prefix_hash: digest.to_vec(),
                tier: PTier::Unspecified.into(),
                size_bytes: 0,
                owner_node: String::new(),
                last_access_unix_ms: 0,
            },
        };
        Ok(Response::new(info))
    }

    async fn batch_lookup(&self, req: Request<HashList>) -> Result<Response<BlockInfoList>, Status> {
        let hl = req.into_inner();
        let entries: Vec<BlockInfo> = hl.values.iter().filter_map(|v| {
            let digest = digest_from_bytes(v).ok()?;
            let addr = cgn_kv::BlockAddress { digest, layer: 0 };
            self.store.lookup(&addr).map(|h| BlockInfo {
                prefix_hash: digest.to_vec(),
                tier: tier_to_proto(h.meta.tier).into(),
                size_bytes: h.meta.bytes,
                owner_node: String::new(),
                last_access_unix_ms: h.meta.last_seen_unix * 1000,
            })
        }).collect();
        Ok(Response::new(BlockInfoList { entries }))
    }

    async fn promote(&self, req: Request<Hash>) -> Result<Response<PStatus>, Status> {
        let h = req.into_inner();
        let digest = digest_from_bytes(&h.value)?;
        let addr = cgn_kv::BlockAddress { digest, layer: 0 };
        // Promotion currently means: ensure the block is in RAM. With only
        // RAM + index tiers wired, this is a no-op when present and a
        // miss otherwise.
        let resp = match self.store.lookup(&addr) {
            Some(_) => PStatus { code: 0, message: "promoted".into() },
            None    => PStatus { code: 5, message: "miss".into() },
        };
        Ok(Response::new(resp))
    }

    async fn push(&self, req: Request<PushSpec>) -> Result<Response<PStatus>, Status> {
        let spec = req.into_inner();
        let digest = digest_from_bytes(&spec.prefix_hash)?;
        let addr = cgn_kv::BlockAddress { digest, layer: 0 };
        let bytes = match self.store.ram.get_bytes(&addr) {
            Some(b) => b,
            None => return Ok(Response::new(PStatus {
                code: 5,
                message: "block not resident".into(),
            })),
        };
        let remote: SocketAddr = match spec.target_endpoint.parse() {
            Ok(a) => a,
            Err(e) => return Ok(Response::new(PStatus {
                code: 3, message: format!("bad target_endpoint: {e}"),
            })),
        };
        match crate::transport::peer_push(remote, addr, bytes).await {
            Ok(()) => Ok(Response::new(PStatus { code: 0, message: "pushed".into() })),
            Err(e) => Ok(Response::new(PStatus {
                code: 14, message: format!("push: {e}"),
            })),
        }
    }

    async fn pull(&self, req: Request<PullSpec>) -> Result<Response<PStatus>, Status> {
        let spec = req.into_inner();
        let digest = digest_from_bytes(&spec.prefix_hash)?;
        let addr = cgn_kv::BlockAddress { digest, layer: 0 };
        let remote: SocketAddr = match spec.source_endpoint.parse() {
            Ok(a) => a,
            Err(e) => return Ok(Response::new(PStatus {
                code: 3, message: format!("bad source_endpoint: {e}"),
            })),
        };
        match crate::transport::peer_pull(remote, addr).await {
            Ok(bytes) if bytes.is_empty() => Ok(Response::new(PStatus {
                code: 5, message: "peer returned empty".into(),
            })),
            Ok(bytes) => {
                if let Err(e) = self.store.put_ram(addr, bytes, "") {
                    return Ok(Response::new(PStatus {
                        code: 13, message: format!("local insert: {e}"),
                    }));
                }
                Ok(Response::new(PStatus { code: 0, message: "pulled".into() }))
            }
            Err(e) => Ok(Response::new(PStatus {
                code: 14, message: format!("pull: {e}"),
            })),
        }
    }

    async fn stats(&self, _req: Request<StatsRequest>) -> Result<Response<StatsResponse>, Status> {
        Ok(Response::new(StatsResponse {
            ram_used_bytes: self.store.ram.used_bytes(),
            ram_cap_bytes:  self.store.ram.capacity_bytes(),
            ssd_used_bytes: self.store.ssd.used_bytes(),
            ssd_cap_bytes:  self.store.ssd.capacity(),
            hot_blocks: 0,
            warm_blocks: self.store.ram.block_count() as u64,
            cold_blocks: 0,
            hits: 0, misses: 0, evictions: 0,
            bytes_pushed: 0, bytes_pulled: 0,
        }))
    }
}

fn digest_from_bytes(b: &[u8]) -> Result<[u8; 32], Status> {
    if b.len() != 32 {
        return Err(Status::invalid_argument("hash must be 32 bytes"));
    }
    let mut d = [0u8; 32]; d.copy_from_slice(b); Ok(d)
}

fn tier_to_proto(t: cgn_kv::TierKind) -> PTier {
    match t {
        cgn_kv::TierKind::Gpu => PTier::Gpu,
        cgn_kv::TierKind::Ram => PTier::Ram,
        cgn_kv::TierKind::Ssd => PTier::Ssd,
    }
}

// `Tier` trait is brought into scope via the `cgn_kv::tier::Tier` impls
// on `RamTier`. The unused import keeps clippy quiet about the call.
#[allow(unused_imports)]
use cgn_kv::tier::Tier as _;
