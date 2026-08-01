{{/*
Read-only ServiceAccount/ClusterRole/ClusterRoleBinding for the portal's
dashboard to read Deployment readiness across the cluster (see
crates/adapters/k8s). Only the portal chart should set
`dashboardRbac.enabled: true` -- no other tool needs cluster-wide read
access.
*/}}
{{- define "tool-library.dashboardRbac" -}}
{{- if .Values.dashboardRbac.enabled }}
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: {{ include "tool-library.fullname" . }}-readonly
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
rules:
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: {{ include "tool-library.fullname" . }}-readonly
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: {{ include "tool-library.fullname" . }}-readonly
subjects:
  - kind: ServiceAccount
    name: {{ include "tool-library.fullname" . }}
    namespace: {{ .Release.Namespace }}
{{- end }}
{{- end -}}
