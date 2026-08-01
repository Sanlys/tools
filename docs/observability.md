# Observability

Every tool gets Prometheus metrics, alerting rules, and a Grafana
dashboard for free by setting `monitoring.enabled: true` -- assuming your
cluster already runs the Prometheus Operator (kube-prometheus-stack) and
grafana-operator, which this scaffolding assumes but doesn't install.

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

**Your Prometheus Operator install almost certainly only watches
`ServiceMonitor`s matching a specific label** (its
`serviceMonitorSelector`). Set `monitoring.serviceMonitorLabels` in the
tool's `values.yaml` to match it, or the ServiceMonitor will be created but
silently never scraped.

## Alerting

`prometheusrule.yaml` renders two starter alerts per tool: `<Name>Down`
(no successful scrape for 5 minutes) and `<Name>HighErrorRate` (5xx rate
above 5% for 10 minutes, using the `axum_http_requests_total` metric
`axum-prometheus` produces). Same caveat as above:
`monitoring.prometheusRuleLabels` must match your Prometheus Operator's
`ruleSelector`, and the alert expressions assume `job` equals the
Service/ServiceMonitor name (the kube-prometheus-stack default -- adjust
if your relabeling differs).

## Dashboard

`grafanadashboard.yaml` renders a `GrafanaDashboard`
(`grafana.integreatly.org/v1beta1`, grafana-operator) with three panels:
request rate by status code, p99 latency, and an up/down stat. Set
`monitoring.grafanaInstanceSelector` to match your Grafana CR's
`instanceSelector` labels, or grafana-operator won't attach the dashboard
to any Grafana instance.

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
This only needs the portal's own `dashboardRbac.enabled: true`
(read-only, cluster-wide `get`/`list`/`watch` on Deployments) -- it doesn't
talk to Prometheus at all.

This per-tool fan-out runs sequentially and uncached today, which stops
scaling gracefully well before the tool count gets large -- see
`docs/scaling-and-limitations.md`.
