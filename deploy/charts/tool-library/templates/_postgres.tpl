{{/*
A single-instance Postgres for apps that need one: a Deployment (strategy
Recreate, since a lone ReadWriteOnce PVC can't be mounted by two pods at
once during a rollout), a PVC, a Service, and a Secret with both the split
POSTGRES_* vars and a precomputed DATABASE_URL. Deliberately not HA and not
operator-managed -- see crates/adapters/postgres for why.
*/}}
{{- define "tool-library.postgres" -}}
{{- if .Values.postgres.enabled }}
{{- $fullname := include "tool-library.fullname" . -}}
{{- $db := .Values.postgres.database | default (include "tool-library.name" .) -}}
{{- $user := include "tool-library.name" . -}}
{{- $password := include "tool-library.postgresPassword" . -}}
{{- $host := printf "%s-postgres" $fullname -}}
apiVersion: v1
kind: Secret
metadata:
  name: {{ $fullname }}-postgres
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
type: Opaque
stringData:
  POSTGRES_USER: {{ $user }}
  POSTGRES_PASSWORD: {{ $password }}
  POSTGRES_DB: {{ $db }}
  POSTGRES_HOST: {{ $host }}
  POSTGRES_PORT: "5432"
  DATABASE_URL: "postgres://{{ $user }}:{{ $password }}@{{ $host }}:5432/{{ $db }}"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ $fullname }}-postgres
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app.kubernetes.io/name: {{ include "tool-library.name" . }}-postgres
      app.kubernetes.io/instance: {{ .Release.Name }}
  template:
    metadata:
      labels:
        app.kubernetes.io/name: {{ include "tool-library.name" . }}-postgres
        app.kubernetes.io/instance: {{ .Release.Name }}
    spec:
      containers:
        - name: postgres
          image: {{ .Values.postgres.image | default "postgres:16-alpine" }}
          ports:
            - containerPort: 5432
              name: postgres
          envFrom:
            - secretRef:
                name: {{ $fullname }}-postgres
          volumeMounts:
            - name: data
              mountPath: /var/lib/postgresql/data
              subPath: pgdata
          readinessProbe:
            exec:
              command: ["pg_isready", "-U", {{ $user | quote }}]
            initialDelaySeconds: 5
            periodSeconds: 10
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: {{ $fullname }}-postgres
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {{ $fullname }}-postgres
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  accessModes: ["ReadWriteOnce"]
  {{- if .Values.postgres.storageClassName }}
  storageClassName: {{ .Values.postgres.storageClassName }}
  {{- end }}
  resources:
    requests:
      storage: {{ .Values.postgres.storage | default "1Gi" }}
---
apiVersion: v1
kind: Service
metadata:
  name: {{ $host }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  selector:
    app.kubernetes.io/name: {{ include "tool-library.name" . }}-postgres
    app.kubernetes.io/instance: {{ .Release.Name }}
  ports:
    - port: 5432
      targetPort: postgres
{{- end }}
{{- end -}}
