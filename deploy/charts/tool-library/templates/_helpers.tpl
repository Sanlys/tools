{{/*
Standard name/label helpers, following the usual `helm create` conventions
so anything that expects them (kubectl -l, dashboards, etc.) keeps working.
*/}}

{{- define "tool-library.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "tool-library.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "tool-library.labels" -}}
app.kubernetes.io/name: {{ include "tool-library.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "tool-library.selectorLabels" -}}
app.kubernetes.io/name: {{ include "tool-library.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Postgres password: deterministic (a SHA-256 hash of the release namespace
+ fullname), not random. This used to read the existing Secret back via
`lookup` and only fall back to `randAlphaNum` when nothing was found --
that only stays stable under a real `helm upgrade` with live cluster
access. ArgoCD renders Helm charts via `helm template`, where `lookup`
never has cluster access and always returns empty -- so every sync
(including a plain self-heal reconciliation with no git changes)
regenerated a brand-new random password and overwrote the Secret, while
the already-initialized Postgres data volume kept whatever password was
baked in at first boot. Result: password authentication failures that
show up some time after first deploy, not immediately. A value derived
only from stable inputs (no cluster state, no randomness) can never drift
like that. This isn't a meaningfully weaker secret than before: Postgres
here is only ever reachable inside its own namespace, and anyone who can
read the Secret already has the password in cleartext either way.
*/}}
{{- define "tool-library.postgresPassword" -}}
{{- printf "%s/%s/postgres-password" .Release.Namespace (include "tool-library.fullname" .) | sha256sum | trunc 32 -}}
{{- end -}}
