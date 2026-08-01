{{/*
Tells the cluster's Prometheus Operator to scrape this app's /metrics
(exposed by crates/adapters/metrics via axum-prometheus). Most
kube-prometheus-stack installs only pick up ServiceMonitors matching a
specific label on the Prometheus CR's `serviceMonitorSelector` -- set
`monitoring.serviceMonitorLabels` to match yours if it doesn't pick this up.
*/}}
{{- define "tool-library.servicemonitor" -}}
{{- if .Values.monitoring.enabled }}
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
    {{- with .Values.monitoring.serviceMonitorLabels }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
spec:
  selector:
    matchLabels:
      {{- include "tool-library.selectorLabels" . | nindent 6 }}
  endpoints:
    - port: http
      path: /metrics
      interval: {{ .Values.monitoring.interval | default "30s" }}
{{- end }}
{{- end -}}
