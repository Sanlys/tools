{{/*
Restricts ingress to whichever Gateway namespace fronts this tool
(httpRoute.gatewayNamespace) - locks the selected pod from default-allow
to default-deny-except-listed (standard Cilium behaviour: selecting an
endpoint for ingress flips it, no separate default-deny policy needed).
Opt-in via networkPolicy.enabled, default false, so existing apps don't
change behaviour just by picking up a chart update.

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
{{- end }}
{{- end -}}
