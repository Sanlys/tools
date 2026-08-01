{{/*
The app's own Deployment. Wires in envFrom for the bucket Secret/ConfigMap
and the Postgres Secret automatically when `bucket.enabled`/`postgres.enabled`
are set -- an app just reads the well-known env var names (see
crates/adapters/{s3,postgres}) rather than wiring any of this by hand.
*/}}
{{- define "tool-library.deployment" -}}
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  replicas: {{ .Values.replicaCount | default 1 }}
  selector:
    matchLabels:
      {{- include "tool-library.selectorLabels" . | nindent 6 }}
  template:
    metadata:
      labels:
        {{- include "tool-library.selectorLabels" . | nindent 8 }}
      {{- if .Values.monitoring.enabled }}
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "{{ .Values.containerPort }}"
        prometheus.io/path: "/metrics"
      {{- end }}
    spec:
      {{- if .Values.serviceAccount.create }}
      serviceAccountName: {{ .Values.serviceAccount.name | default (include "tool-library.fullname" .) }}
      {{- end }}
      {{- with .Values.imagePullSecrets }}
      imagePullSecrets:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      containers:
        - name: {{ include "tool-library.name" . }}
          image: "{{ .Values.image.repository }}:{{ .Values.image.tag }}"
          imagePullPolicy: {{ .Values.image.pullPolicy | default "IfNotPresent" }}
          ports:
            - name: http
              containerPort: {{ .Values.containerPort }}
          envFrom:
            {{- if .Values.bucket.enabled }}
            - configMapRef:
                name: {{ include "tool-library.fullname" . }}-bucket
            - secretRef:
                name: {{ include "tool-library.fullname" . }}-bucket
            {{- end }}
            {{- if .Values.postgres.enabled }}
            - secretRef:
                name: {{ include "tool-library.fullname" . }}-postgres
            {{- end }}
            {{- with .Values.envFrom }}
            {{- toYaml . | nindent 12 }}
            {{- end }}
          {{- with .Values.env }}
          env:
            {{- toYaml . | nindent 12 }}
          {{- end }}
          livenessProbe:
            httpGet:
              path: /health
              port: http
            initialDelaySeconds: 5
            periodSeconds: 15
          readinessProbe:
            httpGet:
              path: /health
              port: http
            initialDelaySeconds: 3
            periodSeconds: 10
          resources:
            {{- toYaml (.Values.resources | default dict) | nindent 12 }}
{{- end -}}
