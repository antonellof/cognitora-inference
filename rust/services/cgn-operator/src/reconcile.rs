//! Shared context passed to every controller's reconcile fn.

use kube::Client;

#[derive(Clone)]
pub struct Ctx {
    pub client: Client,
}
