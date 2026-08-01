{{/*
A minimal starter dashboard (request rate, p99 latency, up/down) via
grafana-operator's GrafanaDashboard CRD. `instanceSelector` must match the
labels on your Grafana CR -- set `monitoring.grafanaInstanceSelector` to
whatever that is; the default here is just a common convention, not a
guarantee it matches your install.
*/}}
{{- define "tool-library.grafanadashboard" -}}
{{- if .Values.monitoring.enabled }}
apiVersion: grafana.integreatly.org/v1beta1
kind: GrafanaDashboard
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  instanceSelector:
    matchLabels:
      {{- toYaml .Values.monitoring.grafanaInstanceSelector.matchLabels | nindent 6 }}
  json: |
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
