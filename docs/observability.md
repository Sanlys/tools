# Observability

Every tool gets Prometheus metrics, alerting rules, and a Grafana
dashboard for free by setting `monitoring.enabled: true` -- this assumes
the cluster's Prometheus Operator (kube-prometheus-stack) and Grafana
(same chart, sidecar dashboard provisioning), both already running; see
`cluster/prod/platform/monitoring` in the `kubernetes` repo.

## Metrics endpoint

`crates/adapters/metrics::metrics_layer()` wraps `axum-prometheus`: merge
the returned router and layer into your app, and `/metrics` exposes
request-count/latency/in-flight histograms in Prometheus text format
automatically for every route.

```rust
let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer();
let app = Router::new()
    .route("/health", get(health))
    .merge(metrics_router)
    .layer(metrics_layer)
    .with_state(state);
```

## Scraping

`deploy/charts/tool-library`'s `servicemonitor.yaml` template renders a
`ServiceMonitor` (`monitoring.coreos.com/v1`) pointed at the app's Service,
scraping `/metrics` on the interval set by `monitoring.interval` (default
`30s`).

This cluster's Prometheus Operator watches `ServiceMonitor`s/
`PrometheusRule`s cluster-wide with an empty selector
(`serviceMonitorSelector: {}` in `kube-prometheus-stack`'s values), so
`monitoring.serviceMonitorLabels`/`prometheusRuleLabels` can stay empty --
they're there in case that selector ever gets narrowed.

## Alerting

`prometheusrule.yaml` renders two starter alerts per tool: `<Name>Down`
(no successful scrape for 5 minutes) and `<Name>HighErrorRate` (5xx rate
above 5% for 10 minutes, using the `axum_http_requests_total` metric
`axum-prometheus` produces). The alert expressions assume `job` equals the
Service/ServiceMonitor name (the kube-prometheus-stack default).

## Dashboard

There's no grafana-operator/`GrafanaDashboard` CRD in this cluster.
`grafanadashboard.yaml` instead renders a plain `ConfigMap` labeled
`grafana_dashboard: "1"` in the `monitoring` namespace
(`monitoring.dashboardNamespace`) -- Grafana's bundled sidecar
(kube-prometheus-stack) picks it up automatically from there. Three
panels: request rate by status code, p99 latency, and an up/down stat.

## The platform dashboard (in `apps/portal`)

Separate from the above -- this is the "basic dashboard" in the unified
app itself, not Grafana. `apps/portal/backend`'s `GET /api/status`
computes, per tool in the registry:

- an HTTP hit against `<api_base_url><health_path>` (default `/health`),
  timed
- Kubernetes Deployment readiness via `crates/adapters/k8s`, if
  `k8s_namespace`/`k8s_deployment` are set for that tool in
  `deploy/portal/values.yaml`'s `TOOLS_REGISTRY_JSON`

and combines them into a `Healthy`/`Degraded`/`Down`/`Unknown` verdict,
which `DashboardPanel` in the portal's egui UI polls every 10 seconds.
It doesn't talk to Prometheus at all.

Reading Deployment readiness needs RBAC, but not a cluster-wide
`ClusterRole`: the portal sets `serviceAccount.create: true` to get its own
ServiceAccount, and each tool it should be able to see opts in with
`dashboardGrant.enabled: true` in *that tool's own* `values.yaml`, pointing
at the portal's ServiceAccount name/namespace. That renders a namespaced
`Role`/`RoleBinding` in the tool's own namespace only, granting
`get`/`list`/`watch` on Deployments there -- least privilege, one grant per
tool, rather than the portal holding read access to every namespace in the
cluster.

This per-tool fan-out runs sequentially and uncached today, which stops
scaling gracefully well before the tool count gets large -- see
`docs/scaling-and-limitations.md`.
