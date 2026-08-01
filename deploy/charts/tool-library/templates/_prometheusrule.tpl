{{/*
Two starter alerts: the target is unreachable, or its 5xx rate is above 5%.
`job` is assumed to equal the ServiceMonitor/Service name, which is the
kube-prometheus-stack default -- adjust the expressions if your relabeling
config differs.
*/}}
{{- define "tool-library.prometheusrule" -}}
{{- if and .Values.monitoring.enabled .Values.monitoring.alerts.enabled }}
{{- $job := include "tool-library.fullname" . }}
apiVersion: monitoring.coreos.com/v1
kind: PrometheusRule
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
    {{- with .Values.monitoring.prometheusRuleLabels }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
spec:
  groups:
    - name: {{ include "tool-library.fullname" . }}.rules
      rules:
        - alert: {{ include "tool-library.name" . }}Down
          expr: up{job="{{ $job }}"} == 0
          for: 5m
          labels:
            severity: warning
          annotations:
            summary: "{{ include "tool-library.name" . }} is down"
            description: "No successful Prometheus scrape of {{ $job }}'s /metrics for 5 minutes."
        - alert: {{ include "tool-library.name" . }}HighErrorRate
          expr: >-
            sum(rate(axum_http_requests_total{job="{{ $job }}", status=~"5.."}[5m]))
            /
            sum(rate(axum_http_requests_total{job="{{ $job }}"}[5m])) > 0.05
          for: 10m
          labels:
            severity: warning
          annotations:
            summary: "{{ include "tool-library.name" . }} error rate above 5%"
{{- end }}
{{- end -}}
