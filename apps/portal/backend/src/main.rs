//! Backend for the unified portal: serves the compiled wasm UI, the runtime
//! tool registry it fetches on load, and the dashboard's aggregated status
//! endpoint.
//!
//! The tool registry is deliberately *not* baked into the wasm binary --
//! it's read here from a JSON file (or inline env var) at startup, so the
//! same wasm build works across dev/staging/prod. See
//! `deploy/portal/values.yaml` for how the Helm chart populates it from a
//! ConfigMap.

use api_types::{CheckResult, DashboardStatus, Health, ToolRegistry, ToolStatus};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    registry: Arc<ToolRegistry>,
    http_client: reqwest::Client,
    k8s_client: Option<kube::Client>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let registry = Arc::new(load_registry()?);

    let k8s_client = match k8s_adapter::client_from_env().await {
        Ok(client) => Some(client),
        Err(err) => {
            tracing::warn!(
                "no Kubernetes client available ({err}); dashboard will report k8s readiness \
                 as unknown for every tool. Expected when running locally outside the cluster."
            );
            None
        }
    };

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let state = AppState {
        registry,
        http_client,
        k8s_client,
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer();
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./dist".to_string());
    let index_html = format!("{static_dir}/index.html");
    let static_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index_html));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/config/tools.json", get(get_tools))
        .route("/api/status", get(get_status))
        .merge(metrics_router)
        .layer(metrics_layer)
        .layer(cors)
        .with_state(state)
        .fallback_service(static_service);

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("portal-backend listening on {addr}, serving static assets from {static_dir}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Reads the tool registry from, in order: `TOOLS_REGISTRY_JSON` (an
/// inline JSON array, set directly by `deploy/portal/values.yaml`'s
/// `toolsRegistry`), `TOOLS_REGISTRY_FILE` (a path to the same JSON), or a
/// small built-in dev default pointing at the `hello` tool's local ports so
/// `cargo run` works out of the box without any cluster config.
fn load_registry() -> anyhow::Result<ToolRegistry> {
    if let Ok(json) = std::env::var("TOOLS_REGISTRY_JSON") {
        return Ok(serde_json::from_str(&json)?);
    }

    let path = std::env::var("TOOLS_REGISTRY_FILE")
        .unwrap_or_else(|_| "/etc/portal/tools.json".to_string());
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        Err(_) => {
            tracing::warn!(
                "{path} not found, using a built-in dev registry pointing at localhost -- see \
                 deploy/portal/values.yaml for how this is populated in a real deployment"
            );
            Ok(serde_json::from_str(include_str!("dev_registry.json"))?)
        }
    }
}

async fn get_tools(State(state): State<AppState>) -> Json<ToolRegistry> {
    Json((*state.registry).clone())
}

async fn get_status(State(state): State<AppState>) -> Json<DashboardStatus> {
    let mut statuses = Vec::with_capacity(state.registry.len());
    for link in state.registry.iter() {
        let health_path = link
            .health_path
            .clone()
            .unwrap_or_else(|| "/health".to_string());
        let url = format!("{}{}", link.api_base_url.trim_end_matches('/'), health_path);

        let started = Instant::now();
        let http_check = match state.http_client.get(&url).send().await {
            Ok(resp) => Some(CheckResult {
                healthy: resp.status().is_success(),
                latency_ms: Some(started.elapsed().as_millis() as u64),
                message: None,
            }),
            Err(err) => Some(CheckResult {
                healthy: false,
                latency_ms: None,
                message: Some(err.to_string()),
            }),
        };

        let k8s_readiness = match (&state.k8s_client, &link.k8s_namespace, &link.k8s_deployment) {
            (Some(client), Some(namespace), Some(deployment)) => {
                k8s_adapter::deployment_readiness(client, namespace, deployment)
                    .await
                    .map_err(|err| {
                        tracing::warn!("k8s readiness check failed for {}: {err}", link.id)
                    })
                    .ok()
            }
            _ => None,
        };

        let overall = overall_health(&http_check, &k8s_readiness);

        statuses.push(ToolStatus {
            id: link.id.clone(),
            name: link.name.clone(),
            overall,
            http_check,
            k8s_readiness,
            checked_at: chrono::Utc::now().to_rfc3339(),
        });
    }
    Json(statuses)
}

fn overall_health(
    http_check: &Option<CheckResult>,
    k8s_readiness: &Option<api_types::K8sReadiness>,
) -> Health {
    if let Some(http) = http_check {
        if !http.healthy {
            return Health::Down;
        }
    }
    if let Some(k8s) = k8s_readiness {
        if k8s.ready_replicas < k8s.desired_replicas {
            return Health::Degraded;
        }
    }
    match http_check {
        Some(http) if http.healthy => Health::Healthy,
        Some(_) => Health::Down,
        None => Health::Unknown,
    }
}
