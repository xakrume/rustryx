{{/*
Names, labels and small derived values shared by all templates.
*/}}

{{- define "rustryx.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "rustryx.fullname" -}}
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

{{- define "rustryx.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Selector labels carry a component. The migration Job pods share name and instance with the
server pods; without `component: server` in every selector the Service would route traffic to a
migration pod (it has no readiness probe and is "Ready" as soon as it runs).
*/}}
{{- define "rustryx.selectorLabels" -}}
app.kubernetes.io/name: {{ include "rustryx.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: server
{{- end -}}

{{- define "rustryx.labels" -}}
helm.sh/chart: {{ include "rustryx.chart" . }}
{{ include "rustryx.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: rustryx
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/* Labels of the migration Job pods: same name and instance, different component. */}}
{{- define "rustryx.migrateLabels" -}}
helm.sh/chart: {{ include "rustryx.chart" . }}
app.kubernetes.io/name: {{ include "rustryx.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: migrate
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: rustryx
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "rustryx.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "rustryx.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "rustryx.secretName" -}}
{{- required "secrets.existingSecret must name the Secret that holds the credentials" .Values.secrets.existingSecret -}}
{{- end -}}

{{/*
Image reference. A digest wins over the tag. The variant picks the repository and tag suffix
because CI publishes musl and glibc as separate manifests that must never share a tag.
`latest` is refused here as well as in values.schema.json: Helm can be run with
--skip-schema-validation, and OPS-18 must hold either way.
*/}}
{{- define "rustryx.image" -}}
{{- $img := .Values.image -}}
{{- $cfg := get $img.variants $img.variant -}}
{{- if not $cfg -}}
{{- fail (printf "image.variant must be one of %s, got %q" (keys $img.variants | sortAlpha | join ", ") $img.variant) -}}
{{- end -}}
{{- $repo := printf "%s%s" $img.repository (default "" $cfg.repositorySuffix) -}}
{{- if contains ":" (last (splitList "/" $repo)) -}}
{{- fail "image.repository must not contain a tag; use image.tag or image.digest" -}}
{{- end -}}
{{- if $img.digest -}}
{{- if not (regexMatch "^sha256:[a-f0-9]{64}$" $img.digest) -}}
{{- fail "image.digest must look like sha256:<64 lowercase hex characters>" -}}
{{- end -}}
{{- printf "%s@%s" $repo $img.digest -}}
{{- else -}}
{{- if $img.requireDigest -}}
{{- fail "image.requireDigest is true: set image.digest (sha256:...) so the release is pinned to the signed image" -}}
{{- end -}}
{{- $tag := default .Chart.AppVersion $img.tag -}}
{{- if eq (lower $tag) "latest" -}}
{{- fail "image tag \"latest\" is not allowed (OPS-18): use a semver or sha-<commit> tag, or pin image.digest" -}}
{{- end -}}
{{- $suffix := default "" $cfg.tagSuffix -}}
{{- if and $suffix (hasSuffix $suffix $tag) -}}
{{- printf "%s:%s" $repo $tag -}}
{{- else -}}
{{- printf "%s:%s%s" $repo $tag $suffix -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Replica floor and ceiling the cluster can actually reach, used by several validations. */}}
{{- define "rustryx.minReplicas" -}}
{{- if .Values.autoscaling.enabled -}}{{ int .Values.autoscaling.minReplicas }}{{- else -}}{{ int .Values.replicaCount }}{{- end -}}
{{- end -}}

{{- define "rustryx.maxReplicas" -}}
{{- if .Values.autoscaling.enabled -}}{{ int .Values.autoscaling.maxReplicas }}{{- else -}}{{ int .Values.replicaCount }}{{- end -}}
{{- end -}}

{{/* True ("true") when the local blob store sits on a volume more than one pod may mount. */}}
{{- define "rustryx.localIsRWX" -}}
{{- if and (eq .Values.storage.type "local") .Values.storage.local.confirmReadWriteMany (or .Values.storage.local.existingClaim (has "ReadWriteMany" .Values.storage.local.accessModes)) -}}true{{- end -}}
{{- end -}}

{{/*
Port of the probe and metrics listener. One listener serves everything today, so this is the API
port; a dedicated ops listener (containerPorts.ops) takes over probes, metrics Service and policy.
*/}}
{{- define "rustryx.opsPort" -}}
{{- if .Values.containerPorts.ops -}}{{ int .Values.containerPorts.ops }}{{- else -}}{{ int .Values.containerPorts.http }}{{- end -}}
{{- end -}}

{{/* Name of the container port that carries probes and metrics. */}}
{{- define "rustryx.opsPortName" -}}
{{- if .Values.containerPorts.ops -}}ops{{- else -}}http{{- end -}}
{{- end -}}

{{- define "rustryx.pvcName" -}}
{{- default (printf "%s-blobs" (include "rustryx.fullname" .)) .Values.storage.local.existingClaim -}}
{{- end -}}

{{- define "rustryx.terminationGracePeriodSeconds" -}}
{{- if .Values.terminationGracePeriodSeconds -}}
{{- .Values.terminationGracePeriodSeconds -}}
{{- else -}}
{{- add (int .Values.shutdown.preStopSleepSeconds) (int .Values.shutdown.timeoutSeconds) (int .Values.shutdown.graceSlackSeconds) -}}
{{- end -}}
{{- end -}}

{{/*
Env var names of the secrets the operator's Secret is expected to contain, as a JSON list.
The names come from the key table of the rustryx-config crate (docs/config.md): `a.b.c` is
RUSTRYX_A_B_C and a secret mounted as a file is RUSTRYX_A_B_C_FILE.
*/}}
{{- define "rustryx.secretEnvNames" -}}
{{- $e := .Values.secrets.entries -}}
{{- $out := list -}}
{{- if $e.databaseUrl }}{{- $out = append $out "RUSTRYX_DATABASE_URL" -}}{{- end -}}
{{- if $e.databaseDirectUrl }}{{- $out = append $out "RUSTRYX_DATABASE_DIRECT_URL" -}}{{- end -}}
{{- if $e.lockRedisUrl }}{{- $out = append $out "RUSTRYX_LOCK_REDIS_URL" -}}{{- end -}}
{{- if $e.s3AccessKey }}{{- $out = append $out "RUSTRYX_BLOB_S3_ACCESS_KEY" -}}{{- end -}}
{{- if $e.s3SecretKey }}{{- $out = append $out "RUSTRYX_BLOB_S3_SECRET_KEY" -}}{{- end -}}
{{- if $e.oidcClientSecret }}{{- $out = append $out "RUSTRYX_AUTH_OIDC_CLIENT_SECRET" -}}{{- end -}}
{{- if $e.ldapBindPassword }}{{- $out = append $out "RUSTRYX_AUTH_LDAP_BIND_PASSWORD" -}}{{- end -}}
{{- if $e.databaseSealingKey }}{{- $out = append $out "RUSTRYX_DATABASE_SEALING_KEY" -}}{{- end -}}
{{- if $e.databaseSealingPreviousKeys }}{{- $out = append $out "RUSTRYX_DATABASE_SEALING_PREVIOUS_KEYS" -}}{{- end -}}
{{- if $e.internalJwtKey }}{{- $out = append $out "RUSTRYX_AUTH_INTERNAL_JWT_KEY" -}}{{- end -}}
{{- if $e.internalJwtPreviousKeys }}{{- $out = append $out "RUSTRYX_AUTH_INTERNAL_JWT_PREVIOUS_KEYS" -}}{{- end -}}
{{- if $e.cookieKey }}{{- $out = append $out "RUSTRYX_AUTH_COOKIE_KEY" -}}{{- end -}}
{{- if $e.bootstrapAdminPassword }}{{- $out = append $out "RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD" -}}{{- end -}}
{{- if $e.metricsToken }}{{- $out = append $out "RUSTRYX_OBSERVABILITY_METRICS_TOKEN" -}}{{- end -}}
{{- toJson $out -}}
{{- end -}}

{{/*
Environment and envFrom shared by the server container and the migration Job, so the Job runs
with the same settings and the same secrets as the pods it prepares the schema for.
*/}}
{{- define "rustryx.commonEnv" -}}
- name: RUSTRYX_CONFIG
  value: /etc/rustryx/config/config.yaml
{{- if .Values.tls.customCA.enabled }}
- name: SSL_CERT_FILE
  value: {{ printf "%s/%s" .Values.tls.customCA.mountPath .Values.tls.customCA.key | quote }}
{{- end }}
{{- if eq .Values.secrets.mode "file" }}
{{- range $name := (include "rustryx.secretEnvNames" . | fromJsonArray) }}
- name: {{ $name }}_FILE
  value: {{ printf "%s/%s" $.Values.secrets.mountPath $name | quote }}
{{- end }}
{{- end }}
{{- end -}}

{{- define "rustryx.commonEnvFrom" -}}
{{- if eq .Values.secrets.mode "env" }}
- secretRef:
    name: {{ include "rustryx.secretName" . }}
{{- end }}
{{- end -}}

{{/* Volume mounts and volumes common to server and Job (config, tmp, secrets, CA). */}}
{{- define "rustryx.commonVolumeMounts" -}}
# A directory of its own: the custom CA mounts under /etc/rustryx/ca, and a mount nested inside a
# read-only ConfigMap volume is fragile (the runtime must create the mount point in a read-only parent).
- name: config
  mountPath: /etc/rustryx/config
  readOnly: true
- name: tmp
  mountPath: /tmp
{{- if eq .Values.secrets.mode "file" }}
- name: secrets
  mountPath: {{ .Values.secrets.mountPath }}
  readOnly: true
{{- end }}
{{- if .Values.tls.customCA.enabled }}
- name: custom-ca
  mountPath: {{ .Values.tls.customCA.mountPath }}
  readOnly: true
{{- end }}
{{- end -}}

{{- define "rustryx.commonVolumes" -}}
- name: tmp
  emptyDir:
    sizeLimit: {{ .Values.tmp.sizeLimit | quote }}
    {{- with .Values.tmp.medium }}
    medium: {{ . }}
    {{- end }}
{{- if eq .Values.secrets.mode "file" }}
- name: secrets
  secret:
    secretName: {{ include "rustryx.secretName" . }}
    # 0440: readable by the pod group (fsGroup 10001) only.
    defaultMode: 288
{{- end }}
{{- if .Values.tls.customCA.enabled }}
- name: custom-ca
  {{- if .Values.tls.customCA.existingSecret }}
  secret:
    secretName: {{ .Values.tls.customCA.existingSecret }}
  {{- else }}
  configMap:
    name: {{ .Values.tls.customCA.existingConfigMap }}
  {{- end }}
{{- end }}
{{- end -}}

{{/* One egress rule: peers plus TCP ports. Used for every dependency in networkpolicy.yaml. */}}
{{- define "rustryx.egressRule" -}}
- to:
    {{- toYaml .to | nindent 4 }}
  ports:
    {{- range .ports }}
    - protocol: TCP
      port: {{ . }}
    {{- end }}
{{- end -}}
