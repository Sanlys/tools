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
Postgres password: generated once and then kept stable across `helm
upgrade` by reading it back out of the existing Secret, if any. On a plain
`helm template`/dry-run there's nothing to look up, so it generates a fresh
one each time -- that's fine, it's only ever used for the real apply.
*/}}
{{- define "tool-library.postgresPassword" -}}
{{- $secretName := printf "%s-postgres" (include "tool-library.fullname" .) -}}
{{- $existing := lookup "v1" "Secret" .Release.Namespace $secretName -}}
{{- if $existing -}}
{{- index $existing.data "POSTGRES_PASSWORD" | b64dec -}}
{{- else -}}
{{- randAlphaNum 24 -}}
{{- end -}}
{{- end -}}
