//! Wire types shared between backends and the egui/wasm frontends.
//!
//! Keep this crate dependency-light (serde only) since it gets pulled into
//! the wasm build of every tool's frontend as well as every axum backend.

use serde::{Deserialize, Serialize};

/// One entry in the platform-wide tool registry, served by the portal
/// backend at `/config/tools.json` and fetched at *runtime* by the portal's
/// egui/wasm frontend (not baked in at compile time) so the same wasm build
/// works unmodified across dev/staging/prod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLink {
    /// Must match the `Panel::id()` of the corresponding compiled-in panel.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Public base URL of the tool's own standalone deployment, e.g.
    /// `https://hello.tools.example.com`. Used both for the portal's "open
    /// standalone" links and as the default backend base URL for the tool's
    /// embedded panel.
    pub standalone_url: String,
    /// Base URL the tool's *backend API* is reachable on from the browser
    /// running the portal wasm app. Usually the same host as
    /// `standalone_url` but split out in case a tool's UI and API are on
    /// different hosts.
    pub api_base_url: String,
    /// Path appended to `api_base_url` for the dashboard's HTTP health
    /// check. Defaults to `/health` when omitted.
    #[serde(default)]
    pub health_path: Option<String>,
    /// Namespace/Deployment name pair the dashboard reads Kubernetes
    /// readiness from. Leave both unset to skip the k8s check for this
    /// tool (e.g. when running outside the cluster).
    #[serde(default)]
    pub k8s_namespace: Option<String>,
    #[serde(default)]
    pub k8s_deployment: Option<String>,
}

pub type ToolRegistry = Vec<ToolLink>;

/// Health of a single backend as reported by the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

/// One source of truth the dashboard combines into a tool's overall status:
/// an HTTP health-check hit directly, and/or Kubernetes Deployment
/// readiness read from the cluster API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatus {
    pub id: String,
    pub name: String,
    pub overall: Health,
    pub http_check: Option<CheckResult>,
    pub k8s_readiness: Option<K8sReadiness>,
    /// RFC3339 timestamp of when this status was computed.
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sReadiness {
    pub namespace: String,
    pub deployment: String,
    pub ready_replicas: i32,
    pub desired_replicas: i32,
}

pub type DashboardStatus = Vec<ToolStatus>;
