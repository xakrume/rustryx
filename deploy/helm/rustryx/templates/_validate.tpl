{{/*
Render-time validation. Every rule here replaces a failure that would otherwise show up on a live
cluster as a hang, a silent timeout or a pod that cannot start. values.schema.json covers types
and enums; this file covers the cross-field rules a schema cannot express and the rules that must
hold even with `--skip-schema-validation`.
*/}}
{{- define "rustryx.validate" -}}
{{- $v := .Values -}}
{{- $prod := eq $v.config.environment "production" -}}
{{- $minReplicas := int (include "rustryx.minReplicas" .) -}}
{{- $maxReplicas := int (include "rustryx.maxReplicas" .) -}}
{{- $e := $v.secrets.entries -}}
{{- $oidc := $v.config.auth.oidc -}}
{{- $ldap := $v.config.auth.ldap -}}
{{- $obs := $v.config.observability -}}
{{- $redisLock := eq $v.config.lock.backend "redis" -}}

{{- /* image: the helper fails on latest, a malformed digest and a missing required digest */ -}}
{{- $_ := include "rustryx.image" . -}}
{{- $_ = include "rustryx.secretName" . -}}

{{- /* exposure */ -}}
{{- if and $v.ingress.enabled $v.gateway.enabled (not $v.gateway.allowWithIngress) -}}
{{- fail "ingress.enabled and gateway.enabled are mutually exclusive: two entry points to one Service double the TLS and body-size configuration to keep in sync. Set gateway.allowWithIngress=true to run both deliberately." -}}
{{- end -}}
{{- if and $v.gateway.enabled (not $v.gateway.parentRefs) -}}
{{- fail "gateway.enabled requires gateway.parentRefs (the Gateway the HTTPRoute attaches to)" -}}
{{- end -}}

{{- /* base URL: OIDC redirects and Docker token realms are built from it */ -}}
{{- if not $v.config.server.base_url -}}
{{- fail "config.server.base_url is required (external URL of the service)" -}}
{{- end -}}
{{- if and $prod (not (hasPrefix "https://" $v.config.server.base_url)) -}}
{{- fail "config.server.base_url must be https:// when config.environment is production" -}}
{{- end -}}

{{- /* storage */ -}}
{{- if not (has $v.storage.type (list "s3" "local")) -}}
{{- fail (printf "storage.type must be s3 or local, got %q" $v.storage.type) -}}
{{- end -}}
{{- if eq $v.storage.type "s3" -}}
{{- if not (and $v.storage.s3.bucket $v.storage.s3.endpoint $v.storage.s3.region) -}}
{{- fail "storage.type=s3 requires storage.s3.endpoint, bucket and region (the application requires all three)" -}}
{{- end -}}
{{- end -}}
{{- if and (eq $v.storage.type "local") (gt $maxReplicas 1) (not (include "rustryx.localIsRWX" .)) -}}
{{- fail (printf "storage.type=local with up to %d replicas is not supported: a local blob store is incompatible with HA. Use S3, or a ReadWriteMany volume with storage.local.confirmReadWriteMany=true (and ReadWriteMany in storage.local.accessModes or an existingClaim)." $maxReplicas) -}}
{{- end -}}

{{- /* replicas */ -}}
{{- if and $prod (lt $minReplicas 2) (not $v.allowSingleReplica) -}}
{{- fail "config.environment=production requires at least 2 replicas (replicaCount or autoscaling.minReplicas). Set allowSingleReplica=true only for a deliberate single-node setup." -}}
{{- end -}}
{{- if $v.autoscaling.enabled -}}
{{- if lt (int $v.autoscaling.minReplicas) (int $v.replicaCount) -}}
{{- fail "autoscaling.minReplicas must not be below replicaCount (OPS-32)" -}}
{{- end -}}
{{- if lt (int $v.autoscaling.maxReplicas) (int $v.autoscaling.minReplicas) -}}
{{- fail "autoscaling.maxReplicas must be >= autoscaling.minReplicas" -}}
{{- end -}}
{{- end -}}

{{- /* disruption budget: a budget that cannot be satisfied blocks every node drain */ -}}
{{- if $v.pdb.enabled -}}
{{- $hasMin := not (kindIs "invalid" $v.pdb.minAvailable) -}}
{{- $hasMax := not (kindIs "invalid" $v.pdb.maxUnavailable) -}}
{{- if and $hasMin $hasMax -}}
{{- fail "pdb.minAvailable and pdb.maxUnavailable are mutually exclusive: set exactly one (the other to null)" -}}
{{- end -}}
{{- if not (or $hasMin $hasMax) -}}
{{- fail "pdb.enabled requires pdb.minAvailable or pdb.maxUnavailable" -}}
{{- end -}}
{{- if and $hasMin (not (kindIs "string" $v.pdb.minAvailable)) (ge (int $v.pdb.minAvailable) $minReplicas) -}}
{{- fail (printf "pdb.minAvailable=%v is not below the minimum replica count %d: the budget would block every node drain (OPS-32)" $v.pdb.minAvailable $minReplicas) -}}
{{- end -}}
{{- if and $hasMax (not (kindIs "string" $v.pdb.maxUnavailable)) (lt (int $v.pdb.maxUnavailable) 1) -}}
{{- fail "pdb.maxUnavailable must be at least 1, otherwise no pod may ever be evicted" -}}
{{- end -}}
{{- end -}}

{{- /* secrets */ -}}
{{- if not (has $v.secrets.mode (list "env" "file")) -}}
{{- fail (printf "secrets.mode must be env or file, got %q" $v.secrets.mode) -}}
{{- end -}}
{{- if not $e.databaseUrl -}}
{{- fail "secrets.entries.databaseUrl must be true: the node cannot start without database credentials (RUSTRYX_DATABASE_URL)" -}}
{{- end -}}
{{- if and $prod (not (and $e.internalJwtKey $e.cookieKey $e.bootstrapAdminPassword)) -}}
{{- fail "config.environment=production requires secrets.entries internalJwtKey, cookieKey and bootstrapAdminPassword: the application refuses to start without them" -}}
{{- end -}}
{{- $metricsOn := or (not (hasKey $obs "metrics_enabled")) $obs.metrics_enabled -}}
{{- if and $metricsOn (not $obs.metrics_public) (not $e.metricsToken) -}}
{{- fail "/metrics needs a Bearer token unless config.observability.metrics_public is true: set secrets.entries.metricsToken=true (RUSTRYX_OBSERVABILITY_METRICS_TOKEN)" -}}
{{- end -}}
{{- if $v.externalSecret.enabled -}}
{{- if not $v.externalSecret.secretStoreRef.name -}}
{{- fail "externalSecret.secretStoreRef.name is required when externalSecret.enabled" -}}
{{- end -}}
{{- if not $v.externalSecret.data -}}
{{- fail "externalSecret.data is empty: list the secretKey and remoteRef of every entry" -}}
{{- end -}}
{{- end -}}

{{- /* locks: one backend per process life, and Redis is reachable only through its secret URL */ -}}
{{- if not (has $v.config.lock.backend (list "postgres" "redis")) -}}
{{- fail (printf "config.lock.backend must be postgres or redis, got %q" $v.config.lock.backend) -}}
{{- end -}}
{{- if and $redisLock (not $e.lockRedisUrl) -}}
{{- fail "config.lock.backend=redis requires secrets.entries.lockRedisUrl=true (RUSTRYX_LOCK_REDIS_URL; the URL may carry a password, so it is a secret)" -}}
{{- end -}}

{{- /* identity providers: enabled but not configured is an error in the app too (OPS-51) */ -}}
{{- if $oidc.enabled -}}
{{- if not (and $oidc.issuer $oidc.client_id) -}}
{{- fail "config.auth.oidc.enabled requires issuer and client_id" -}}
{{- end -}}
{{- if not (hasPrefix "https://" $oidc.issuer) -}}
{{- fail "config.auth.oidc.issuer must be https://" -}}
{{- end -}}
{{- if not $e.oidcClientSecret -}}
{{- fail "config.auth.oidc.enabled requires secrets.entries.oidcClientSecret=true (confidential client)" -}}
{{- end -}}
{{- end -}}
{{- if $ldap.enabled -}}
{{- if not (and $ldap.url $ldap.bind_dn) -}}
{{- fail "config.auth.ldap.enabled requires url and bind_dn" -}}
{{- end -}}
{{- if not $e.ldapBindPassword -}}
{{- fail "config.auth.ldap.enabled requires secrets.entries.ldapBindPassword=true" -}}
{{- end -}}
{{- end -}}
{{- /* Even a disabled LDAP block must not hold a plain URL: the application rejects it at startup. */ -}}
{{- if $ldap.url -}}
{{- if not (or (hasPrefix "ldaps://" $ldap.url) (and (hasPrefix "ldap://" $ldap.url) $ldap.starttls)) -}}
{{- fail "config.auth.ldap.url must be ldaps://, or ldap:// together with starttls=true (README 8.4: plain ldap:// prevents startup)" -}}
{{- end -}}
{{- end -}}

{{- /* credentials must never ride inside a URL or sit in a key that lands in the ConfigMap */ -}}
{{- $urls := dict "config.server.base_url" $v.config.server.base_url "config.auth.oidc.issuer" $oidc.issuer "config.auth.oidc.public_issuer_url" (default "" $oidc.public_issuer_url) "config.auth.ldap.url" $ldap.url "config.observability.otlp_endpoint" (default "" $obs.otlp_endpoint) "storage.s3.endpoint" (default "" $v.storage.s3.endpoint) -}}
{{- range $key, $url := $urls -}}
{{- if regexMatch "://[^/@[:space:]]*:[^/@[:space:]]*@" (toString $url) -}}
{{- fail (printf "%s contains credentials (user:password@). Put the secret in the Secret named by secrets.existingSecret, never in values or the ConfigMap." $key) -}}
{{- end -}}
{{- end -}}
{{- $rendered := include "rustryx.configYaml" . | fromYaml -}}
{{- range $path := list (list "database" "url") (list "database" "direct_url") (list "lock" "redis_url") (list "blob" "s3_access_key") (list "blob" "s3_secret_key") (list "auth" "internal_jwt_key") (list "auth" "cookie_key") (list "auth" "bootstrap_admin" "password") (list "auth" "oidc" "client_secret") (list "auth" "ldap" "bind_password") (list "observability" "metrics_token") -}}
{{- $found := "" -}}
{{- if eq (len $path) 2 -}}
{{- $found = dig (index $path 0) (index $path 1) "" $rendered -}}
{{- else -}}
{{- $found = dig (index $path 0) (index $path 1) (index $path 2) "" $rendered -}}
{{- end -}}
{{- if $found -}}
{{- fail (printf "config.%s is a secret and must not be set in values or the ConfigMap: deliver it as RUSTRYX_%s through the Secret named by secrets.existingSecret" (join "." $path) (upper (join "_" $path))) -}}
{{- end -}}
{{- end -}}

{{- /* network policy: every enabled dependency needs an egress peer, otherwise it silently times out */ -}}
{{- if $v.networkPolicy.enabled -}}
{{- $eg := $v.networkPolicy.egress -}}
{{- if not $eg.postgresql.to -}}
{{- fail "networkPolicy.egress.postgresql.to is empty: the pods could not reach PostgreSQL. Set the peer, or disable networkPolicy.enabled deliberately." -}}
{{- end -}}
{{- if and $redisLock (not $eg.redis.to) -}}
{{- fail "config.lock.backend=redis but networkPolicy.egress.redis.to is empty" -}}
{{- end -}}
{{- if and (eq $v.storage.type "s3") (not $eg.s3.to) -}}
{{- fail "storage.type=s3 but networkPolicy.egress.s3.to is empty" -}}
{{- end -}}
{{- if and $oidc.enabled (not $eg.keycloak.to) -}}
{{- fail "config.auth.oidc.enabled but networkPolicy.egress.keycloak.to is empty" -}}
{{- end -}}
{{- if and $ldap.enabled (not $eg.ldap.to) -}}
{{- fail "config.auth.ldap.enabled but networkPolicy.egress.ldap.to is empty" -}}
{{- end -}}
{{- end -}}
{{- if and $v.networkPolicy.cilium.enabled (not $v.networkPolicy.cilium.upstreamHosts) -}}
{{- fail "networkPolicy.cilium.enabled requires networkPolicy.cilium.upstreamHosts" -}}
{{- end -}}

{{- /* Docker subdomain connector */ -}}
{{- if and $v.config.docker.subdomainConnector.enabled (not $v.config.docker.subdomainConnector.baseDomain) -}}
{{- fail "config.docker.subdomainConnector.enabled requires config.docker.subdomainConnector.baseDomain" -}}
{{- end -}}

{{- /* TLS CA */ -}}
{{- if $v.tls.customCA.enabled -}}
{{- if not (or $v.tls.customCA.existingSecret $v.tls.customCA.existingConfigMap) -}}
{{- fail "tls.customCA.enabled requires existingSecret or existingConfigMap" -}}
{{- end -}}
{{- end -}}

{{- /* monitoring: a scrape without the token the app demands is a permanent 401 and a silent gap */ -}}
{{- if not (has $v.monitoring.flavor (list "prometheus" "victoriametrics")) -}}
{{- fail (printf "monitoring.flavor must be prometheus or victoriametrics, got %q" $v.monitoring.flavor) -}}
{{- end -}}
{{- if and $v.monitoring.serviceMonitor.enabled $metricsOn (not $obs.metrics_public) -}}
{{- if not (or $v.monitoring.serviceMonitor.bearerTokenSecret.name $e.metricsToken) -}}
{{- fail "monitoring.serviceMonitor.enabled with a protected /metrics needs a token source: set monitoring.serviceMonitor.bearerTokenSecret or secrets.entries.metricsToken" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Probe renderer. Arguments: dict "probe" <values block> "name" <probe name> "root" <chart root>.
Called once per probe, outside any range over service ports (OPS-33). The port is forced to the
listener's containerPort: it is the single source, so a probe can never target the Service port.
*/}}
{{- define "rustryx.probe" -}}
{{- $root := .root -}}
{{- $port := int (include "rustryx.opsPort" $root) -}}
{{- $p := deepCopy (omit .probe "enabled") -}}
{{- if hasKey $p "initialDelaySeconds" -}}
{{- if $root.Values.probes.startup.enabled -}}
{{- fail (printf "probes.%s.initialDelaySeconds must not be set while the startupProbe is enabled (OPS-33): the startupProbe holds the other probes back until the app is up." .name) -}}
{{- end -}}
{{- end -}}
{{- $get := $p.httpGet | default dict -}}
{{- if and (hasKey $get "port") (ne (int $get.port) $port) -}}
{{- fail (printf "probes.%s.httpGet.port must equal the container port of the listener (%d) or be omitted: the probe targets the container port, not the Service port" .name $port) -}}
{{- end -}}
{{- if not $get.path -}}
{{- fail (printf "probes.%s.httpGet.path is required" .name) -}}
{{- end -}}
{{- if contains "health" (lower $get.path) -}}
{{- if not (or (hasSuffix "/healthz" $get.path) (hasSuffix "/readyz" $get.path)) -}}
{{- fail (printf "probes.%s must use /healthz or /readyz, never an aggregated health endpoint" .name) -}}
{{- end -}}
{{- end -}}
{{- $_ := set $get "port" $port -}}
{{- $_ = set $get "scheme" "HTTP" -}}
{{- $_ = set $p "httpGet" $get -}}
{{ toYaml $p }}
{{- end -}}
