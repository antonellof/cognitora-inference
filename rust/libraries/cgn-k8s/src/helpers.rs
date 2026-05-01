//! Small helpers built on top of `kube`.

use cgn_core::Result;
use kube::api::{Api, ListParams};
use kube::core::NamespaceResourceScope;
use kube::Client;
use serde::de::DeserializeOwned;

/// Build a `kube::Client` honouring `KUBECONFIG` / in-cluster service account.
pub async fn client() -> Result<Client> {
    Client::try_default()
        .await
        .map_err(|e| cgn_core::Error::Unavailable(format!("kube client: {e}")))
}

/// Namespaced list helper, returns objects sorted by `metadata.name`.
pub async fn list_namespaced<T>(client: Client, ns: &str) -> Result<Vec<T>>
where
    T: kube::Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + DeserializeOwned
        + Clone
        + std::fmt::Debug,
{
    let api: Api<T> = Api::namespaced(client, ns);
    let mut items = api.list(&ListParams::default()).await
        .map_err(|e| cgn_core::Error::Unavailable(format!("kube list: {e}")))?
        .items;
    items.sort_by(|a, b| a.meta().name.cmp(&b.meta().name));
    Ok(items)
}
