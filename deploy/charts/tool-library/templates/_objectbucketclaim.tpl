{{/*
Declares the app's S3 bucket. Nothing else has to happen: rook-ceph's
bucket provisioner watches ObjectBucketClaims and creates the real Ceph RGW
bucket plus a dedicated, bucket-scoped user, then writes a ConfigMap and
Secret *of the same name* with the connection details -- which is exactly
what crates/adapters/s3 reads via envFrom on this Deployment. There's no
platform-side operator or API service involved; rook does all of it.

`generateBucketName` (rather than a fixed `bucketName`) appends a random
suffix so bucket names don't collide across namespaces/environments -- see
docs/s3-buckets.md.
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
  generateBucketName: {{ .Values.bucket.bucketName }}
  storageClassName: {{ .Values.bucket.storageClassName }}
{{- end }}
{{- end -}}
