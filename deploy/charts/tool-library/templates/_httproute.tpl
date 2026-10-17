{{/*
Gateway API counterpart to tool-library.ingress, for clusters fronted by a
Gateway API controller (e.g. Traefik) instead of/alongside classic Ingress.
Set `httpRoute.enabled` and leave `ingress.enabled` false - see
docs/architecture.md's cluster-specific section.
*/}}
{{- define "tool-library.httproute" -}}
{{- if .Values.httpRoute.enabled }}
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  parentRefs:
    - name: {{ .Values.httpRoute.gatewayName }}
      namespace: {{ .Values.httpRoute.gatewayNamespace }}
      sectionName: websecure
  hostnames: ["{{ .Values.httpRoute.hostname }}"]
  rules:
    - backendRefs:
        - name: {{ include "tool-library.fullname" . }}
          port: 80
{{- end }}
{{- end -}}
