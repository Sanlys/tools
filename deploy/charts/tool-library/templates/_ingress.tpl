{{/*
Deliberately ingress-controller-agnostic: no annotations are assumed by
default. Set `ingress.className`/`ingress.annotations` to match whatever's
actually fronting your cluster (Traefik, nginx-ingress, ...) -- see
docs/architecture.md for the "adjust to your cluster" checklist.
*/}}
{{- define "tool-library.ingress" -}}
{{- if .Values.ingress.enabled }}
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
  {{- with .Values.ingress.annotations }}
  annotations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
spec:
  {{- if .Values.ingress.className }}
  ingressClassName: {{ .Values.ingress.className }}
  {{- end }}
  {{- if .Values.ingress.tls.enabled }}
  tls:
    - hosts:
        - {{ .Values.ingress.host }}
      secretName: {{ .Values.ingress.tls.secretName }}
  {{- end }}
  rules:
    - host: {{ .Values.ingress.host }}
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: {{ include "tool-library.fullname" . }}
                port:
                  name: http
{{- end }}
{{- end -}}
