{{- define "matos.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "matos.fullname" -}}
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

{{- define "matos.componentName" -}}
{{- printf "%s-%s" (include "matos.fullname" .root) .component | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "matos.selectorLabels" -}}
app.kubernetes.io/name: {{ include "matos.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "matos.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .root.Chart.Name .root.Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "matos.selectorLabels" . }}
app.kubernetes.io/version: {{ .root.Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .root.Release.Service }}
app.kubernetes.io/part-of: matos
{{- with .root.Values.global.labels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "matos.image" -}}
{{- $repo := default (printf "%s-%s" .root.Values.image.prefix .component) .values.image.repository -}}
{{- printf "%s:%s" $repo (default .root.Chart.AppVersion .values.image.tag) -}}
{{- end -}}

{{- define "matos.secretEnv" -}}
{{- $root := .root -}}
{{- range .keys }}
- name: {{ . }}
  valueFrom:
    secretKeyRef:
      name: {{ $root.Values.secrets.existingSecret | quote }}
      key: {{ . | quote }}
      {{- if has . $root.Values.secrets.optionalKeys }}
      optional: true
      {{- end }}
{{- end }}
{{- end -}}

{{- define "matos.imagePullSecrets" -}}
{{- with .Values.global.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{- define "matos.topologySpread" -}}
{{- if .root.Values.topologySpread.enabled }}
topologySpreadConstraints:
  - maxSkew: 1
    topologyKey: kubernetes.io/hostname
    whenUnsatisfiable: ScheduleAnyway
    labelSelector:
      matchLabels:
        {{- include "matos.selectorLabels" . | nindent 8 }}
{{- end }}
{{- end -}}

{{- define "matos.scheduling" -}}
{{- with .nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{- define "matos.analyticsClaim" -}}
{{- default (include "matos.componentName" (dict "root" . "component" "analytics")) .Values.analytics.persistence.existingClaim -}}
{{- end -}}

{{- define "matos.vectorStoreClaim" -}}
{{- default (include "matos.componentName" (dict "root" . "component" "vector-store")) .Values.worker.vectorStore.persistence.existingClaim -}}
{{- end -}}

{{- define "matos.mcpUrl" -}}
{{- if .Values.worker.mcpUrl -}}
{{- .Values.worker.mcpUrl -}}
{{- else -}}
{{- printf "http://%s.%s.svc:%d/mcp" (include "matos.componentName" (dict "root" . "component" "mcp-kanban")) .Release.Namespace (int .Values.mcpKanban.service.port) -}}
{{- end -}}
{{- end -}}

{{- define "matos.egressDependency" -}}
- ports:
    {{- range .ports }}
    - protocol: TCP
      port: {{ . }}
    {{- end }}
  {{- with .to }}
  to:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}

{{- define "matos.egressDns" -}}
- to:
    {{- toYaml .Values.networkPolicy.dnsTo | nindent 4 }}
  ports:
    - protocol: UDP
      port: 53
    - protocol: TCP
      port: 53
{{- end -}}

{{/* GitHub hosts, set for GitHub Enterprise only; the App itself is created by each organization from the console. */}}
{{- define "matos.githubBases" -}}
{{- with .Values.config.github.apiBase }}
GITHUB_API_BASE: {{ . | quote }}
{{- end }}
{{- with .Values.config.github.webBase }}
GITHUB_WEB_BASE: {{ . | quote }}
{{- end }}
{{- end }}
