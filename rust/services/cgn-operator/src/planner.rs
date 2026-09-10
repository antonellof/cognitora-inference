//! SLA planner-lite — reactive queue-depth autoscaling for ModelPools.
//!
//! Cognitora 0.7. A `ModelPool` may declare an `slo` block
//! (`maxQueuePerReplica`, `minReplicas`, `maxReplicas`,
//! `scaleCooldownSecs`). Every 30 s this loop reads the live agent
//! heartbeats from etcd (`/cognitora/nodes/<node_id>`, the same
//! `NodeHealth` JSON the router consumes), sums `queue_depth` across
//! ready nodes serving the pool's model, and sizes the pool so that no
//! replica carries more than `maxQueuePerReplica` queued requests:
//!
//! ```text
//! desired = clamp(ceil(total_queue / max_queue_per_replica), min, max)
//! ```
//!
//! Decisions patch `spec.decode_replicas` — queue depth is a decode-side
//! signal, and prefill capacity stays user-controlled. A per-pool
//! cooldown (in-memory; a restart just means one extra cooldown window)
//! prevents heartbeat noise from thrashing replicas. Pools without an
//! `slo.maxQueuePerReplica` are never touched, so pre-0.7 behavior is
//! unchanged.
//!
//! etcd endpoints come from the standard Cognitora config, same as the
//! autoscaler hint consumer. Without etcd the task exits quietly
//! (there are no heartbeats to plan from).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cgn_core::{Error, Result};
use cgn_k8s::crds::ModelPool;
use etcd_client::GetOptions;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    Client,
};
use serde::Deserialize;
use tracing::{debug, info, warn};

const TICK_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_MIN_REPLICAS: u32 = 1;
const DEFAULT_COOLDOWN_SECS: u64 = 120;
const FIELD_MANAGER: &str = "cgn-operator";

/// The subset of the agent `NodeHealth` heartbeat the planner needs
/// (see `cgn-agent::health::publish_one` for the full shape). Extra
/// fields are ignored; missing fields default so a partially-formed
/// entry (e.g. a pipeline worker with `model: null`) is simply skipped
/// by the model filter rather than failing the whole tick.
#[derive(Debug, Deserialize)]
struct Heartbeat {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    queue_depth: u32,
    #[serde(default)]
    ready: bool,
}

pub async fn run(
    client: Client,
    namespace: Option<String>,
    etcd_endpoints: Vec<String>,
) -> Result<()> {
    if etcd_endpoints.is_empty() {
        info!("no etcd endpoints configured; SLA planner disabled");
        return Ok(());
    }
    info!("SLA planner running");
    // Cooldown bookkeeping, keyed by `<namespace>/<name>`.
    let mut last_scale: HashMap<String, Instant> = HashMap::new();
    loop {
        if let Err(e) = tick(
            &client,
            namespace.as_deref(),
            &etcd_endpoints,
            &mut last_scale,
        )
        .await
        {
            warn!(error=?e, "planner tick failed");
        }
        tokio::time::sleep(TICK_INTERVAL).await;
    }
}

async fn tick(
    client: &Client,
    namespace: Option<&str>,
    endpoints: &[String],
    last_scale: &mut HashMap<String, Instant>,
) -> Result<()> {
    let api: Api<ModelPool> = match namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    let pools = api
        .list(&ListParams::default())
        .await
        .map_err(|e| Error::Unavailable(format!("list model pools: {e}")))?;

    // Only pools that opted in via `slo.maxQueuePerReplica` matter; skip
    // the etcd round-trip entirely when nobody did.
    let slo_pools: Vec<&ModelPool> = pools
        .items
        .iter()
        .filter(|p| {
            p.spec
                .slo
                .as_ref()
                .is_some_and(|s| s.max_queue_per_replica.is_some())
        })
        .collect();
    if slo_pools.is_empty() {
        return Ok(());
    }

    let heartbeats = fetch_heartbeats(endpoints).await?;
    for pool in slo_pools {
        // Per-pool failures (e.g. a conflicting patch) must not starve
        // the other pools of planning.
        if let Err(e) = plan_pool(client, pool, &heartbeats, last_scale).await {
            warn!(pool = %pool.name_any(), error=?e, "planner: pool skipped");
        }
    }
    Ok(())
}

/// Read every node heartbeat under the NODES prefix. Unparsable values
/// are skipped (an old agent mid-upgrade should not break planning).
async fn fetch_heartbeats(endpoints: &[String]) -> Result<Vec<Heartbeat>> {
    let mut client = etcd_client::Client::connect(endpoints, None)
        .await
        .map_err(|e| Error::Etcd(format!("connect: {e}")))?;
    let resp = client
        .get(
            cgn_core::etcd_keys::NODES,
            Some(GetOptions::new().with_prefix()),
        )
        .await
        .map_err(|e| Error::Etcd(format!("get nodes: {e}")))?;
    Ok(resp
        .kvs()
        .iter()
        .filter_map(|kv| serde_json::from_slice::<Heartbeat>(kv.value()).ok())
        .collect())
}

async fn plan_pool(
    client: &Client,
    pool: &ModelPool,
    heartbeats: &[Heartbeat],
    last_scale: &mut HashMap<String, Instant>,
) -> Result<()> {
    let name = pool.name_any();
    let ns = pool.namespace().unwrap_or_else(|| "default".into());
    let slo = pool.spec.slo.as_ref().expect("caller filtered on slo");
    let max_queue = slo.max_queue_per_replica.expect("caller filtered on it");
    if max_queue == 0 {
        warn!(%name, "slo.maxQueuePerReplica is 0; ignoring pool");
        return Ok(());
    }

    let current = pool.spec.decode_replicas;
    let min = slo.min_replicas.unwrap_or(DEFAULT_MIN_REPLICAS);
    // No explicit ceiling → the user-set replica count is the ceiling,
    // so an unbounded queue can never scale a pool past what was asked.
    let max = slo.max_replicas.unwrap_or(current);

    let total_queue: u32 = heartbeats
        .iter()
        .filter(|h| h.ready && h.model.as_deref() == Some(pool.spec.model.as_str()))
        .map(|h| h.queue_depth)
        .sum();

    let desired = desired_replicas(total_queue, max_queue, min, max);
    if desired == current {
        return Ok(());
    }

    let key = format!("{ns}/{name}");
    let cooldown = Duration::from_secs(slo.scale_cooldown_secs.unwrap_or(DEFAULT_COOLDOWN_SECS));
    if let Some(t) = last_scale.get(&key) {
        if t.elapsed() < cooldown {
            debug!(%name, %ns, current, desired, "planner: scale wanted but cooling down");
            return Ok(());
        }
    }

    let api: Api<ModelPool> = Api::namespaced(client.clone(), &ns);
    let pp = PatchParams::apply(FIELD_MANAGER);
    // Merge patch: only touch the one replica field, leaving every other
    // spec field (and other field managers) alone.
    let spec_patch = serde_json::json!({ "spec": { "decode_replicas": desired } });
    api.patch(&name, &pp, &Patch::Merge(&spec_patch))
        .await
        .map_err(|e| Error::Unavailable(format!("patch model pool spec: {e}")))?;

    // Status is observability only; a failure here must not undo or
    // block the scale we just applied.
    let status_patch = serde_json::json!({ "status": {
        "desiredReplicas": desired,
        "lastScaleTime": chrono::Utc::now().to_rfc3339(),
    }});
    if let Err(e) = api
        .patch_status(&name, &pp, &Patch::Merge(&status_patch))
        .await
    {
        warn!(%name, %ns, error=?e, "planner: status patch failed");
    }

    last_scale.insert(key, Instant::now());
    info!(
        %name, %ns, model = %pool.spec.model,
        total_queue, max_queue_per_replica = max_queue,
        from = current, to = desired,
        "planner: scaled ModelPool decode replicas"
    );
    Ok(())
}

/// Pure sizing rule: enough replicas that no one carries more than
/// `max_queue_per_replica` queued requests, clamped to `[min, max]`.
/// `max_queue_per_replica == 0` is treated as 1 (callers reject it, but
/// division by zero must be impossible here); `min > max` resolves in
/// favor of `min` so a misconfigured pool keeps its availability floor.
fn desired_replicas(total_queue: u32, max_queue_per_replica: u32, min: u32, max: u32) -> u32 {
    let per = max_queue_per_replica.max(1);
    total_queue.div_ceil(per).clamp(min, max.max(min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_queue_floors_at_min() {
        assert_eq!(desired_replicas(0, 8, 1, 10), 1);
        assert_eq!(desired_replicas(0, 8, 3, 10), 3);
    }

    #[test]
    fn scales_up_with_queue() {
        // 17 queued / 8 per replica → ceil = 3.
        assert_eq!(desired_replicas(17, 8, 1, 10), 3);
        // Exact multiple needs no extra replica.
        assert_eq!(desired_replicas(16, 8, 1, 10), 2);
        // One over the boundary does.
        assert_eq!(desired_replicas(9, 8, 1, 10), 2);
    }

    #[test]
    fn scales_down_when_queue_shrinks() {
        // Was running high; queue now fits in one replica.
        assert_eq!(desired_replicas(5, 8, 1, 10), 1);
    }

    #[test]
    fn clamps_to_max() {
        assert_eq!(desired_replicas(1000, 8, 1, 4), 4);
    }

    #[test]
    fn zero_per_replica_does_not_divide_by_zero() {
        assert_eq!(desired_replicas(100, 0, 1, 4), 4);
    }

    #[test]
    fn min_wins_over_smaller_max() {
        // Misconfigured (min > max): keep the availability floor.
        assert_eq!(desired_replicas(0, 8, 5, 2), 5);
    }
}
