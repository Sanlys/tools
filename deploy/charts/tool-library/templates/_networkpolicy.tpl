{{/*
Restricts ingress to whichever Gateway namespace fronts this tool
(httpRoute.gatewayNamespace), plus the monitoring namespace when
monitoring.enabled (Prometheus scrapes /metrics directly via
ServiceMonitor - a separate path from the Gateway's traffic entirely,
not covered by the first rule). Locks the selected pod from
default-allow to default-deny-except-listed (standard Cilium behaviour:
selecting an endpoint for ingress flips it, no separate default-deny
policy needed). Opt-in via networkPolicy.enabled, default false, so
existing apps don't change behaviour just by picking up a chart update.

Missing the monitoring rule here once already caused a live TargetDown
alert on game-mgr (Prometheus couldn't reach it once ingress flipped to
default-deny) - reusing monitoring.dashboardNamespace since that's
already set to wherever Prometheus Operator discovers ServiceMonitors
from, even though its name suggests it's only about the Grafana
dashboard ConfigMap.

Only covers the httpRoute path, not classic ingress.enabled - every tool
on this platform uses Gateway API today. Needs a CNI that enforces
CiliumNetworkPolicy (Cilium, which is what the target cluster runs) -
see docs/architecture.md.
*/}}
{{- define "tool-library.networkpolicy" -}}
{{- if .Values.networkPolicy.enabled }}
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: {{ include "tool-library.fullname" . }}-allow-gateway-only
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  endpointSelector:
    matchLabels:
      {{- include "tool-library.selectorLabels" . | nindent 6 }}
  ingress:
    - fromEndpoints:
        - matchLabels:
            k8s:io.kubernetes.pod.namespace: {{ .Values.httpRoute.gatewayNamespace }}
      toPorts:
        - ports:
            - port: "{{ .Values.containerPort }}"
              protocol: TCP
    {{- if .Values.monitoring.enabled }}
    - fromEndpoints:
        - matchLabels:
            k8s:io.kubernetes.pod.namespace: {{ .Values.monitoring.dashboardNamespace }}
      toPorts:
        - ports:
            - port: "{{ .Values.containerPort }}"
              protocol: TCP
    {{- end }}
{{- end }}
{{- end -}}
