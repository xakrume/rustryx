{{/*
Config file rendering. The file holds non-sensitive settings only (OPS-39): secrets reach the
process through the Secret named in secrets.existingSecret and never pass through this template.
*/}}

{{/*
Recursive prune: drops empty strings, empty lists, empty maps and nulls, keeps booleans and
numbers (so `enabled: false` survives). Input and output are wrapped in {v, keep} JSON because
fromJson only decodes objects. Why prune at all: values.yaml lists every optional key with an
empty default for documentation, and an empty `public_issuer_url: ""` would reach the
application as a configured-but-invalid value instead of an absent one.
*/}}
{{- define "rustryx.prune" -}}
{{- $in := . -}}
{{- $keep := true -}}
{{- $val := $in -}}
{{- if kindIs "map" $in -}}
{{- $out := dict -}}
{{- range $k, $v := $in -}}
{{- $r := include "rustryx.prune" $v | fromJson -}}
{{- if $r.keep }}{{- $_ := set $out $k $r.v -}}{{- end -}}
{{- end -}}
{{- $val = $out -}}
{{- if not $out }}{{- $keep = false -}}{{- end -}}
{{- else if kindIs "slice" $in -}}
{{- if not $in }}{{- $keep = false -}}{{- end -}}
{{- else if kindIs "string" $in -}}
{{- if eq $in "" }}{{- $keep = false -}}{{- end -}}
{{- else if kindIs "invalid" $in -}}
{{- $keep = false -}}
{{- end -}}
{{- toJson (dict "v" $val "keep" $keep) -}}
{{- end -}}

{{/*
The config document as YAML, with the application's own key names (docs/config.md). Unknown keys
are errors at startup, so this template writes only keys of that schema. Listen address, shutdown
deadline and the blob store are derived from the values that also drive the pod spec (container
port, grace period, volume mounts), so the file and the pod cannot disagree.
*/}}
{{- define "rustryx.configYaml" -}}
{{- $v := .Values -}}
{{- $cfg := deepCopy (omit $v.config "extra" "docker" "server") -}}
{{- $server := deepCopy $v.config.server -}}
{{- $_ := set $server "listen_addr" (printf "0.0.0.0:%d" (int $v.containerPorts.http)) -}}
{{- $_ = set $server "shutdown_timeout_secs" (int $v.shutdown.timeoutSeconds) -}}
{{- $_ = set $cfg "server" $server -}}
{{- $blob := dict -}}
{{- if eq $v.storage.type "s3" -}}
{{- $_ = set $blob "kind" "s3" -}}
{{- $_ = set $blob "s3_endpoint" (default "" $v.storage.s3.endpoint) -}}
{{- $_ = set $blob "s3_bucket" (default "" $v.storage.s3.bucket) -}}
{{- $_ = set $blob "s3_region" (default "" $v.storage.s3.region) -}}
{{- $_ = set $blob "s3_path_style" $v.storage.s3.path_style -}}
{{- else -}}
{{- $_ = set $blob "kind" "local" -}}
{{- $_ = set $blob "local_path" $v.storage.local.mountPath -}}
{{- end -}}
{{- $_ = set $cfg "blob" $blob -}}
{{- $cfg = mergeOverwrite $cfg (deepCopy $v.config.extra) -}}
{{- $pruned := include "rustryx.prune" $cfg | fromJson -}}
# Rendered by Helm from values `config` and `storage`. Non-sensitive settings only.
# Credentials and keys arrive as environment variables or as files named by <NAME>_FILE.
{{ toYaml $pruned.v }}
{{- end -}}
