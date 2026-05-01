//! One controller per CRD. Each module exposes `run(client, namespace)`
//! that returns once the controller's reconcile loop exits.

pub mod inference_cluster;
pub mod model_pool;
pub mod routing_policy;
