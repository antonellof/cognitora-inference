{{/*
Common helpers for the Cognitora chart.
*/}}
{{- define "cognitora.fullname" -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "cognitora.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "cognitora.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "cognitora.image" -}}
{{- $tag := .Values.global.image.tag | default .Chart.AppVersion -}}
{{- printf "%s/%s:%s" .Values.global.image.registry .Values.global.image.repository $tag -}}
{{- end -}}

{{/* Service that exposes the router's /metrics. */}}
{{- define "cognitora.routerMetricsService" -}}
{{- printf "%s-router-metrics" (include "cognitora.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* `job` label of router scrapes, used by alert rules. */}}
{{- define "cognitora.routerJob" -}}
{{- default (include "cognitora.routerMetricsService" .) .Values.metrics.prometheusRule.routerJob -}}
{{- end -}}
