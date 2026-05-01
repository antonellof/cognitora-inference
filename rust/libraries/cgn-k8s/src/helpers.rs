//! Small helpers built on top of `kube`.

use cgn_core::Result;
use kube::api::{Api, ListParams};
use kube::Client;
use serde::de::DeserializeOwned;

/// Build a `kube::Client` honouring `KUBECONFIG` / in-cluster service account.
pub async fn client() -> Result<Client> {
    Client::try_default()
        .await
        .map_err(|e| cgn_core::Error::Unavailable(format!("kube client: {e}")))
}

/// Cluster-wide list helper, returns objects sorted by `metadata.name`.
pub async fn list_all<T>(client: Client, ns: &str) -> Result<Vec<T>>
where
    T: kube::Resource<DynamicType = ()> + DeserializeOwned + Clone + std::fmt::Debug,
{
    let api: Api<T> = Api::namespaced(client, ns);
    let mut items = api.list(&ListParams::default()).await
        .map_err(|e| cgn_core::Error::Unavailable(format!("kube list: {e}")))?
        .items;
    items.sort_by(|a, b| a.meta().name.cmp(&b.meta().name));
    Ok(items)
}
