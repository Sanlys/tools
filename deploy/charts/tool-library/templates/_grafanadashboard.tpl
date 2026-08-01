{{/*
A minimal starter dashboard (request rate, p99 latency, up/down), provisioned
the kube-prometheus-stack way: a plain ConfigMap labeled `grafana_dashboard:
"1"` that Grafana's sidecar picks up automatically -- there's no
grafana-operator/GrafanaDashboard CRD in this cluster. The sidecar only
watches its own release namespace (`monitoring` by default -- see
cluster/prod/platform/monitoring/grafana-dashboards), so this ConfigMap is
deliberately created there rather than in the app's own namespace; set
`monitoring.dashboardNamespace` if yours differs.
*/}}
{{- define "tool-library.grafanadashboard" -}}
{{- if .Values.monitoring.enabled }}
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ include "tool-library.fullname" . }}-dashboard
  namespace: {{ .Values.monitoring.dashboardNamespace | default "monitoring" }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
    grafana_dashboard: "1"
data:
  {{ include "tool-library.fullname" . }}.json: |
    {{- include "tool-library.grafanaDashboardJson" . | nindent 4 }}
{{- end }}
{{- end -}}

{{- define "tool-library.grafanaDashboardJson" -}}
{{- $job := include "tool-library.fullname" . }}
{
  "title": {{ include "tool-library.name" . | quote }},
  "uid": {{ $job | quote }},
  "schemaVersion": 39,
  "panels": [
    {
      "type": "timeseries",
      "title": "Request rate",
      "gridPos": { "h": 8, "w": 12, "x": 0, "y": 0 },
      "targets": [
        {
          "expr": "sum(rate(axum_http_requests_total{job=\"{{ $job }}\"}[5m])) by (status)",
          "legendFormat": "{{`{{status}}`}}"
        }
      ]
    },
    {
      "type": "timeseries",
      "title": "p99 latency",
      "gridPos": { "h": 8, "w": 12, "x": 12, "y": 0 },
      "targets": [
        {
          "expr": "histogram_quantile(0.99, sum(rate(axum_http_requests_duration_seconds_bucket{job=\"{{ $job }}\"}[5m])) by (le))",
          "legendFormat": "p99"
        }
      ]
    },
    {
      "type": "stat",
      "title": "Up",
      "gridPos": { "h": 4, "w": 6, "x": 0, "y": 8 },
      "targets": [
        { "expr": "up{job=\"{{ $job }}\"}" }
      ]
    }
  ]
}
{{- end -}}
