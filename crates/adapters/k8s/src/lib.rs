//! Read-only Kubernetes client for the dashboard's Deployment-readiness
//! check.
//!
//! This is intentionally narrow: it only ever *reads* Deployment status. The
//! portal backend's ServiceAccount is bound to a `ClusterRole` scoped to
//! `get`/`list`/`watch` on `deployments` only (see
//! `deploy/charts/tool-library/templates/dashboard-rbac.yaml`) -- nothing in
//! this crate needs write access to the cluster.

use api_types::K8sReadiness;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{Api, Client};

#[derive(Debug, thiserror::Error)]
pub enum K8sAdapterError {
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

/// Builds a client from in-cluster config when running as a pod (reads the
/// mounted ServiceAccount token), falling back to the local kubeconfig
/// otherwise -- handy for running the dashboard backend locally against a
/// dev cluster.
pub async fn client_from_env() -> Result<Client, K8sAdapterError> {
    Client::try_default().await.map_err(Into::into)
}

/// Fetches readiness for a single Deployment.
pub async fn deployment_readiness(
    client: &Client,
    namespace: &str,
    deployment_name: &str,
) -> Result<K8sReadiness, K8sAdapterError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = api.get(deployment_name).await?;

    let desired_replicas = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.replicas)
        .unwrap_or(1);
    let ready_replicas = deployment
        .status
        .as_ref()
        .and_then(|status| status.ready_replicas)
        .unwrap_or(0);

    Ok(K8sReadiness {
        namespace: namespace.to_string(),
        deployment: deployment_name.to_string(),
        ready_replicas,
        desired_replicas,
    })
}
