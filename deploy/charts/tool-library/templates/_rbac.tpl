{{/*
ServiceAccount for the app pod. Only needed by tools that other tools grant
access to (currently: the portal, via each tool's `dashboardGrant` below) --
most tools don't need `serviceAccount.create: true` at all and just run as
`default`.
*/}}
{{- define "tool-library.serviceAccount" -}}
{{- if .Values.serviceAccount.create }}
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {{ .Values.serviceAccount.name | default (include "tool-library.fullname" .) }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
{{- end }}
{{- end -}}

{{/*
Namespace-scoped Role/RoleBinding granting one other ServiceAccount (the
portal's, by convention) read-only access to Deployments in *this tool's own
namespace only* -- not a ClusterRole. Set this on an individual tool's
values when you want it to show up in the portal's dashboard (see
crates/adapters/k8s); `dashboardGrant.subjectName`/`subjectNamespace` must
match the portal's actual ServiceAccount (`serviceAccount.create: true` on
the portal's own values).
*/}}
{{- define "tool-library.dashboardGrant" -}}
{{- if .Values.dashboardGrant.enabled }}
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: {{ include "tool-library.fullname" . }}-dashboard-viewer
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
rules:
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: {{ include "tool-library.fullname" . }}-dashboard-viewer
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: {{ include "tool-library.fullname" . }}-dashboard-viewer
subjects:
  - kind: ServiceAccount
    name: {{ .Values.dashboardGrant.subjectName }}
    namespace: {{ .Values.dashboardGrant.subjectNamespace }}
{{- end }}
{{- end -}}
