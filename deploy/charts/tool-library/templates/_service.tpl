{{- define "tool-library.service" -}}
apiVersion: v1
kind: Service
metadata:
  name: {{ include "tool-library.fullname" . }}
  labels:
    {{- include "tool-library.labels" . | nindent 4 }}
spec:
  selector:
    {{- include "tool-library.selectorLabels" . | nindent 4 }}
  ports:
    - name: http
      port: 80
      targetPort: http
{{- end -}}
