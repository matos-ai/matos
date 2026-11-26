{{- define "matos.egressProxyName" -}}
{{- include "matos.componentName" (dict "root" . "component" "egress-proxy") -}}
{{- end -}}

{{- define "matos.egressProxyUrl" -}}
{{- printf "http://%s.%s.svc:%d" (include "matos.egressProxyName" .) .Release.Namespace (int .Values.egressProxy.service.port) -}}
{{- end -}}
