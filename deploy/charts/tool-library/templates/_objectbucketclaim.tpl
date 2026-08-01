{{/*
Declares the app's S3 bucket. Nothing else has to happen: rook-ceph's
bucket provisioner watches ObjectBucketClaims and creates the real Ceph RGW
bucket plus a dedicated, bucket-scoped user, then writes a ConfigMap and
Secret *of the same name* with the connection details -- which is exactly
what crates/adapters/s3 reads via envFrom on this Deployment. There's no
platform-side operator or API service involved; rook does all of it.

A fixed `bucketName` (matching every other ObjectBucketClaim in this
cluster, e.g. media/syncthing's) rather than `generateBucketName` -- one
namespace per tool already makes the name unique, so a random suffix just
makes the bucket harder to reference by a predictable name.
*/}}
{{- define "tool-library.objectbucketclaim" -}}
{{- if .Values.bucket.enabled }}
apiVersion: objectbucket.io/v1alpha1
kind: ObjectBucketClaim
metadata:
  name: {{ include "tool-library.fullname" . }}-bucket
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  bucketName: {{ .Values.bucket.bucketName }}
  storageClassName: {{ .Values.bucket.storageClassName }}
{{- end }}
{{- end -}}
