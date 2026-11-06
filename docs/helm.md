# Helm chart

The chart lives in `deploy/helm/rustryx` (apiVersion v2, no dependencies). It deploys stateless Rustryx nodes.
PostgreSQL, Redis, S3, Keycloak and LDAP are not part of it: they are installed separately. Every default
fails closed and every rule below is enforced at render time or by `values.schema.json`, not by convention.

`values.yaml` is the commented source of truth. This page explains the structure, the rules and the traps; it
lists each top-level key and the keys you will actually change.

## What the chart guarantees

| Guarantee | Where it is enforced |
|---|---|
| No password, key or token in any values file; the chart renders no `Secret` | `test.sh` greps values, `check_rendered.py` rejects a Secret, `_validate.tpl` rejects secret keys in `config` |
| Pods run as 10001:10001, non-root, no privilege escalation, read-only root filesystem, all capabilities dropped, seccomp `RuntimeDefault`, no API token | the schema pins these values; `check_rendered.py` verifies the rendered manifests |
| Only `/tmp` (emptyDir) is writable, plus the PVC of a local blob store | deployment template |
| No `latest` tag, digest pinning supported and required in production values | schema, `rustryx.image` helper, `image.requireDigest` |
| Two replicas minimum in production, `maxUnavailable: 0`, anti-affinity and topology spread | `_validate.tpl`, deployment template |
| Probes come from values, are rendered once outside any `range`, and target the listener's container port | `rustryx.probe` helper, `check_rendered.py` |
| Liveness is `/healthz` (no dependency), readiness is `/readyz` (PostgreSQL only), no aggregated endpoint | `rustryx.probe` helper |
| Schema changes only by the migration hook Job; `serve` never migrates | templates, unit tests |
| Default-deny NetworkPolicy with egress only to named dependencies | `networkpolicy.yaml`, `_validate.tpl` |
| Local blob store plus more than one replica fails the render unless ReadWriteMany is confirmed | `_validate.tpl` |
| Every alert has severity, summary and a runbook link that exists | `check_assets.py`, `rules.yaml` |

## Prerequisites

* Kubernetes 1.30 or newer. Reason: the container has no shell and no `sleep` binary, so the graceful-shutdown
  pause uses the native `preStop.sleep` action, which is GA in 1.30.
* Helm 3.14 or newer (Helm 4 works).
* PostgreSQL 14 or newer reachable from the pods; S3-compatible storage (or one ReadWriteMany volume).
* Optional, only when the matching feature is on: Keycloak, an LDAP server, Redis, the Prometheus operator or
  the VictoriaMetrics operator, kube-state-metrics (for the `rustryx.kube` alert group), External Secrets
  Operator, cert-manager, Cilium, a Gateway API implementation, a metrics adapter for the HPA.

## Install and upgrade

1. Create the Secret in the namespace (or let External Secrets create it). Its keys are listed under
   [The Secret](#the-secret). The chart takes only its name.
2. Write a values file for the site, layered over `values-prod.yaml` (or `values-test.yaml`). Replace every
   placeholder host, CIDR and name in it.
3. Install, pinned by digest in production:

```sh
helm upgrade --install rustryx deploy/helm/rustryx -n rustryx \
  -f values-prod.yaml -f my-site.yaml \
  --set image.digest=sha256:<digest of the signed image>
```

4. Verify the probes on a live pod (`kubectl port-forward` plus `curl`, see the runbook) and read the migration
   Job log.

Image selection. `image.variant` is `musl` (static, distroless/static) or `glibc` (distroless/cc). CI publishes
them as separate manifests and one tag never mixes them, so the variant adds a suffix to the tag
(`0.1.0-musl`, `0.1.0-glibc`); `image.variants.<name>.tagSuffix` and `repositorySuffix` adapt the chart to a
different registry layout. A digest wins over the tag and renders `repository@sha256:...`. `image.tag: latest`
(any case) is rejected by the schema and, for `--skip-schema-validation` runs, by the template.

GitOps. With ArgoCD the migration hook maps to `PreSync`. The chart never renders a Secret, so there is no
`last-applied-configuration` annotation carrying secret material (OPS-39). If you use External Secrets with
GitOps, see the first-install ordering note under [The Secret](#the-secret).

## The Secret

The chart consumes `secrets.existingSecret`, the NAME of a Secret created by the operator, by External Secrets
Operator or by a Vault Agent based injector. The pod has no Vault client and no `VAULT_*` variable; values never
appear in values files, CI variables or git.

Keys are the environment variable names of the configuration schema (`a.b.c` is `RUSTRYX_A_B_C`, see
[config.md](config.md)). `secrets.mode` decides how they reach the process:

* `env` (default): `envFrom.secretRef`. The Secret keys are the variable names.
* `file`: the Secret is mounted read-only at `secrets.mountPath` and the chart sets `<NAME>_FILE` variables
  pointing at the files (the application accepts `RUSTRYX_..._FILE` for every secret key, removes one trailing
  newline, and refuses setting both forms). The values stay out of the process environment and out of crash dumps.

`secrets.entries.*` says which keys the Secret contains. It drives validation, the list printed by `NOTES.txt`
and, in file mode, the `_FILE` variables.

| Entry | Key in the Secret | Default | Required when |
|---|---|---|---|
| `databaseUrl` | `RUSTRYX_DATABASE_URL` | true | always (full DSN with the password) |
| `databaseDirectUrl` | `RUSTRYX_DATABASE_DIRECT_URL` | false | PostgreSQL is behind a transaction-mode pooler (advisory locks and `LISTEN/NOTIFY` need a direct session) |
| `lockRedisUrl` | `RUSTRYX_LOCK_REDIS_URL` | false | `config.lock.backend` is `redis` |
| `s3AccessKey` | `RUSTRYX_BLOB_S3_ACCESS_KEY` | true | S3 storage without a workload identity |
| `s3SecretKey` | `RUSTRYX_BLOB_S3_SECRET_KEY` | true | S3 storage without a workload identity |
| `oidcClientSecret` | `RUSTRYX_AUTH_OIDC_CLIENT_SECRET` | false | `config.auth.oidc.enabled` |
| `ldapBindPassword` | `RUSTRYX_AUTH_LDAP_BIND_PASSWORD` | false | `config.auth.ldap.enabled` |
| `databaseSealingKey` | `RUSTRYX_DATABASE_SEALING_KEY` | true | production (64 hex characters, identical on every node; seals the secrets stored in the database, for example upstream passwords) |
| `databaseSealingPreviousKeys` | `RUSTRYX_DATABASE_SEALING_PREVIOUS_KEYS` | false | rotating the sealing key (`id:hex` lines of retired keys) |
| `internalJwtKey` | `RUSTRYX_AUTH_INTERNAL_JWT_KEY` | true | production (PEM Ed25519, identical on every node) |
| `internalJwtPreviousKeys` | `RUSTRYX_AUTH_INTERNAL_JWT_PREVIOUS_KEYS` | false | rotating the signing key (`kid: <id>` lines each followed by a PEM block) |
| `cookieKey` | `RUSTRYX_AUTH_COOKIE_KEY` | true | production (base64 of 32 random bytes, identical on every node) |
| `bootstrapAdminPassword` | `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD` | true | production, while the database has no administrator yet; at least 12 characters, no known weak value |
| `metricsToken` | `RUSTRYX_OBSERVABILITY_METRICS_TOKEN` | true | `/metrics` is enabled and not public; at least 16 characters in production |

The application refuses to start without these in production and prints every problem at once (exit code 78),
so a missing key shows up in `kubectl logs --previous` of the first crashing pod, not as a silent half start.

External Secrets. `externalSecret.enabled` renders an `ExternalSecret` that fills `secrets.existingSecret`.
`data[].secretKey` is the key name from the table, `data[].remoteRef` the path in the store; no value is ever
written. First-install trap: Helm creates hook resources before regular ones, so on the first install the
Secret does not exist when the migration Job starts. Either create the ExternalSecret first, or set
`externalSecret.hook: true` with `creationPolicy: Orphan` so the ExternalSecret is applied as an early hook (an
`Owner` policy would delete the Secret with the hook on every upgrade). `externalSecret.serviceAccount.create`
makes a separate ServiceAccount for the SecretStore with no token mount and no Role, so a compromised pod does
not inherit access to the secret store.

## Values reference

Top-level keys, in the order of `values.yaml`.

### `image`, `imagePullSecrets`

| Key | Default | Meaning |
|---|---|---|
| `image.repository` | `ghcr.io/rustryx/rustryx` | repository without a tag |
| `image.variant` | `musl` | `musl` or `glibc` |
| `image.variants.<v>.tagSuffix` / `.repositorySuffix` | `-musl`, `-glibc` / empty | how a variant is named in the registry |
| `image.tag` | empty (chart `appVersion`) | never `latest` |
| `image.digest` | empty | `sha256:<64 hex>`; wins over the tag |
| `image.requireDigest` | `false` | refuse to render without a digest (set in `values-prod.yaml`) |
| `image.pullPolicy` | `IfNotPresent` | |
| `imagePullSecrets` | `[]` | names of registry pull Secrets |

### `replicaCount`, `strategy`, `shutdown`, scheduling

| Key | Default | Meaning |
|---|---|---|
| `revisionHistoryLimit` | `5` | old ReplicaSets kept for rollback |
| `replicaCount` | `2` | omitted from the Deployment when `autoscaling.enabled` so upgrades do not reset the HPA |
| `allowSingleReplica` | `false` | lifts the two-replica floor of production mode |
| `strategy` | RollingUpdate, `maxUnavailable: 0`, `maxSurge: 1` | capacity never drops during a rollout |
| `containerPorts.http` | `8081` | the single listener: API, artifacts, probes, metrics; single source for the container port, Service target, listen address in the config and probe port |
| `containerPorts.ops` | `null` | a dedicated probe and metrics listener; see [Single listener](#single-listener-and-the-ops-port) |
| `args` | `[serve]` | the subcommand; `serve` never migrates |
| `shutdown.timeoutSeconds` | `30` | drain deadline, written to `server.shutdown_timeout_secs` |
| `shutdown.preStopSleepSeconds` | `10` | pause after readiness turns 503 so the balancer removes the pod |
| `shutdown.graceSlackSeconds` | `15` | slack on top of drain |
| `terminationGracePeriodSeconds` | `null` | derived as preStop + drain + slack |
| `affinity.podAntiAffinity.mode` | `soft` | `soft`, `hard` or `none` |
| `affinity.custom` | `{}` | a complete `affinity` block that replaces the generated one |
| `topologySpreadConstraints` | zone and hostname, `ScheduleAnyway` | the chart adds the label selector; `whenUnsatisfiable` is a value |
| `priorityClassName`, `nodeSelector`, `tolerations`, `podAnnotations`, `podLabels` | empty | usual meaning |
| `resources` | requests 500m CPU, 512Mi; memory limit 1Gi, no CPU limit | starting values of SRE-33, to be replaced by load test results |
| `tmp.sizeLimit`, `tmp.medium` | `1Gi`, default | the only writable scratch space |
| `podSecurityContext`, `securityContext` | hardened, see above | the schema pins the safe values; they cannot be switched off |
| `serviceAccount.*` | created, no token | annotations carry cloud workload identity |
| `extraEnv`, `extraEnvFrom`, `extraVolumes`, `extraVolumeMounts` | empty | site specifics; never put secret values in `extraEnv` |
| `tls.customCA.*` | off | mount an internal CA from a Secret or ConfigMap and set `SSL_CERT_FILE` |
| `nameOverride`, `fullnameOverride`, `commonLabels` | empty | usual meaning |

Shutdown arithmetic. On SIGTERM the app turns `/readyz` into 503, waits `preStopSleepSeconds` (which must be at
least the readiness `periodSeconds x failureThreshold`, 5 x 2 = 10 s), then stops accepting and drains in-flight
requests until `timeoutSeconds`, then cancels background jobs, releases locks and exits 0. Kubernetes starts the
grace period clock before `preStop` runs, so the pod grace period has to cover the three parts:
`terminationGracePeriodSeconds = 10 + 30 + 15 = 55` by default. The requirements say "drain deadline plus 15"; the
chart adds the preStop pause because it is spent inside the same clock. Raise `shutdown.timeoutSeconds` when
clients push multi-GB blobs through a node; the production values use 120 (145 s grace).

The Service link variables (`RUSTRYX_SERVICE_*`, `RUSTRYX_PORT*`) that Kubernetes injects for a Service named
`rustryx` are switched off with `enableServiceLinks: false`.

### `probes`

Defined once under `probes.startup`, `probes.readiness` and `probes.liveness`; each takes `enabled`,
`httpGet.path` and the timing fields. The port is not a setting: the template writes the container port of the
listener. Rules enforced at render time:

* `initialDelaySeconds` must not be set while the startup probe is enabled.
* A probe port other than the listener's container port fails the render (a probe on the Service port is a
  guaranteed restart loop).
* A path containing `health` that is not `/healthz` or `/readyz` (an aggregated endpoint such as
  `/actuator/health`) fails the render.
* Startup budget: `periodSeconds x failureThreshold` (150 s by default) should be at least three times the cold
  start from the `started in N seconds` log line.

Why readiness depends only on PostgreSQL: without the database a node cannot serve one metadata request, so
leaving the balancer is right; the pod is not restarted because liveness touches no dependency, so when
PostgreSQL returns, all pods return together. Redis, Keycloak, LDAP, S3 and upstream outages are degraded modes
and must not turn into a full outage by emptying the Service.

### `service`, `ingress`, `gateway`

| Key | Default | Meaning |
|---|---|---|
| `service.type` | `ClusterIP` | |
| `service.http.port` | `80` | Service port of the listener |
| `service.ops.port` | `9090` | used only with `containerPorts.ops` |
| `service.extraPorts` | `[]` | `{name, port, targetPort}` |
| `ingress.enabled` | `false` | nginx class by default |
| `ingress.hosts`, `ingress.tls` | example host, none | usual meaning |
| `ingress.certManager.*` | off | adds the cert-manager annotation |
| `ingress.largeUploads.*` | on | nginx annotations: unlimited body (the app enforces `max_blob_bytes`), no request buffering, no response buffering, 600 s timeouts |
| `ingress.docker.hosts` | `[]` | extra hostnames for Docker clients (a registry must sit at the root of a host) |
| `gateway.enabled` | `false` | Gateway API `HTTPRoute` |
| `gateway.parentRefs` | `[]` | required when enabled |
| `gateway.hostnames`, `gateway.docker.hostnames`, `gateway.timeouts` | empty | |
| `gateway.allowWithIngress` | `false` | Ingress and Gateway are mutually exclusive unless this is set |
| `config.docker.subdomainConnector.enabled`, `.baseDomain` | `false`, empty | adds the wildcard host `*.<baseDomain>` to Ingress or HTTPRoute |

Large uploads. Without the `largeUploads` annotations nginx caps bodies at 1 MiB and buffers request bodies on
disk, which breaks `docker push`, `mvn deploy` and `npm publish` of large artifacts. These annotations are written
only for the `nginx` class. A Gateway implementation has its own body-size and buffering policy; configure it on
the Gateway (for example Envoy Gateway's `BackendTrafficPolicy` or NGINX Gateway Fabric's `ClientSettingsPolicy`),
the chart does not know which one you run.

Docker subdomain connector. The flag adds the wildcard host so that `<repo>.<baseDomain>` reaches the Service. It
writes **no application key**: the connector is not in the configuration schema yet and an unknown key stops the
pod. The TLS certificate must be a wildcard for the base domain. Path-based access (`/v2/<repo>/...`) needs no flag.

### `autoscaling`, `pdb`

* `autoscaling.enabled` (default `false`): `autoscaling/v2` on CPU (`cpu.targetAverageUtilization`, 70) and,
  with `requestsPerSecond.enabled`, on a per-pod request rate (`metricName`, `targetAverageValue`).
  `minReplicas` must not be below `replicaCount`. Scale-down is slow (`behavior`, 5 minute window, one pod per
  minute) so a node is not removed in the middle of an upload.
* The request-rate metric needs an adapter (prometheus-adapter or the VictoriaMetrics adapter) that publishes the
  rate of `http_requests_total` under `metricName`. Without it that metric stays unknown and the HPA cannot scale
  down, so enable it only where the adapter exists. A prometheus-adapter rule:

```yaml
rules:
  custom:
    - seriesQuery: 'http_requests_total{job="rustryx",namespace!="",pod!=""}'
      resources:
        overrides:
          namespace: {resource: namespace}
          pod: {resource: pod}
      name:
        matches: "^http_requests_total$"
        as: "http_requests_per_second"
      metricsQuery: 'sum(rate(<<.Series>>{<<.LabelMatchers>>,route!~"/(healthz|readyz|metrics)"}[2m])) by (<<.GroupBy>>)'
```

* `pdb.maxUnavailable: 1` by default (`minAvailable` is the alternative; set exactly one and the other to null).
  A budget that cannot be satisfied (minAvailable not below the replica floor, maxUnavailable 0) fails the render
  because it would block every node drain. `unhealthyPodEvictionPolicy: AlwaysAllow` matters: during a PostgreSQL
  outage every pod is NotReady, and with the default policy the PDB would block node drains at the worst moment.

### `networkPolicy`

Default deny in both directions for the server pods and, separately, for the migration Job (PostgreSQL and DNS
only). Peers are standard `NetworkPolicyPeer` objects.

| Key | Meaning |
|---|---|
| `networkPolicy.ingress.controllers` | who may reach the listener (default: the `ingress-nginx` namespace; for Gateway API give the data plane namespace) |
| `networkPolicy.ingress.metricsScrapers` | who may scrape `/metrics` (default: the `monitoring` namespace); on a single listener this is merged into the same rule |
| `networkPolicy.egress.dns` | DNS, enabled by default, to `kube-dns` |
| `networkPolicy.egress.postgresql` / `redis` / `s3` / `keycloak` / `ldap` | `to` (peers) and `ports` for each dependency |
| `networkPolicy.egress.upstream.cidrs`, `.ports` | CIDR allowlist for proxy repositories |
| `networkPolicy.cilium.enabled`, `.upstreamHosts`, `.upstreamPorts` | optional `CiliumNetworkPolicy` with `toFQDNs` |

An enabled dependency with an empty `to` list fails the render: a missing rule is a silent timeout in production.
Redis is a dependency only when `config.lock.backend` is `redis`.

Upstream registries and the Cilium FQDN trap. Plain NetworkPolicy cannot match host names, so
`egress.upstream.cidrs` is a CIDR list that the operator resolves and keeps current; registries behind CDNs change
addresses, so that list rots. With Cilium, `networkPolicy.cilium` writes `toFQDNs`. Know the limits before you
treat it as a boundary:

* Cilium allows the IPs a name resolved to, per IP identity. When an allowed name shares an IP with other names
  (a CDN edge), every name on that IP becomes reachable. Test with a connection to a host that is **not** listed.
* The policy only works through Cilium's DNS proxy; the chart adds the required `rules.dns` entry. Pods must use
  the cluster DNS.
* Long-lived connections outlive DNS TTLs; an IP that stopped belonging to the name may stay reachable until the
  connection ends.
* The application has its own SSRF filter and host allowlist (`config.upstream.allowed_hosts`,
  `allow_private_cidrs`, validated after DNS resolution and on redirects). Keep both layers.
* A negative test is mandatory on the test contour (OPS-37): a connection to a host outside the allowlist must be
  refused.

### `secrets`, `externalSecret`

See [The Secret](#the-secret).

### `config`

Rendered into `/etc/rustryx/config/config.yaml`, mounted read-only from a ConfigMap. Keys are the application's own
(`config.example.yaml`, [config.md](config.md)); environment variables win over the file. **Unknown keys in the
file are startup errors**, so the chart writes only keys of that schema:

| Source | Rendered as |
|---|---|
| `containerPorts.http` | `server.listen_addr` (`0.0.0.0:<port>`) |
| `shutdown.timeoutSeconds` | `server.shutdown_timeout_secs` |
| `storage.type`, `storage.s3.*`, `storage.local.mountPath` | `blob.kind`, `blob.s3_endpoint`, `blob.s3_bucket`, `blob.s3_region`, `blob.s3_path_style`, `blob.local_path` |
| `config.environment` | `environment` (`production` or `development`) |
| `config.server.*`, `config.database.pool_size`, `config.lock.backend`, `config.auth.*`, `config.limits`, `config.upstream`, `config.gc`, `config.observability.*` | the same keys, unchanged |
| `config.extra` | deep-merged last, for any key not modelled |
| `config.docker.subdomainConnector` | nothing (routing only) |

Empty strings, empty lists and nulls are dropped, booleans and numbers are kept, so an unset optional value
reaches the application as absent rather than as an invalid empty string. Secret keys in `config` are refused at
render time, including inside `extra` (`database.url`, `database.direct_url`, `lock.redis_url`,
`auth.internal_jwt_key`, `auth.cookie_key`, `auth.bootstrap_admin.password`, `auth.oidc.client_secret`,
`auth.ldap.bind_password`, `observability.metrics_token`), as is any URL with `user:password@`.

Things to set deliberately:

* `config.server.base_url`: required, https in production.
* `config.server.trusted_proxies`: the CIDR of the ingress controller's pods or nodes. The login guard and the
  audit log take the client address from `X-Forwarded-For` only when the direct peer is listed; without it they
  see the ingress address.
* `config.auth.ldap.url`: `ldaps://`, or `ldap://` together with `starttls: true`. The CA of the directory is a
  file: mount it with `tls.customCA` and set `config.auth.ldap.ca_file`.
* `config.observability.metrics_public: false` with a token. On a single listener `/metrics` shares the API port,
  so the Bearer token is its only guard; consider denying `/metrics` at the ingress as well.
* `config.environment`: `production` for every contour that holds real data. The test values use it too, so a bad
  Secret fails on test first.

### `storage`

* `storage.type: s3` (default). `storage.s3.endpoint`, `bucket` and `region` are required (the application
  requires all three); `path_style` defaults to `true` (SeaweedFS, Ceph). Keys come from the Secret; turn
  `secrets.entries.s3AccessKey` and `s3SecretKey` off when the pod authenticates through a workload identity.
* `storage.type: local` creates a PVC (`storage.local.size`, `storageClassName`, `accessModes`, `existingClaim`)
  mounted at `storage.local.mountPath`, annotated `helm.sh/resource-policy: keep` so uninstall does not delete
  blobs. A local store is incompatible with HA: with more than one replica (counting the HPA maximum) the chart
  fails unless `accessModes` has `ReadWriteMany` (or an `existingClaim` is given) **and**
  `storage.local.confirmReadWriteMany` is `true`. On a ReadWriteOnce volume the Deployment uses the `Recreate`
  strategy, because a rolling update would wait forever for the volume to detach; every rollout then has a short
  outage.
* The application rejects writes with 507 below `blob.local_min_free_percent` (10%) free space and keeps reading.

### `migrations`

A Helm hook Job `pre-install,pre-upgrade` (PreSync with ArgoCD) running `rustryx migrate`; the process takes a
PostgreSQL advisory lock, so two concurrent runs are safe. Same image, config and Secret as the pods;
`backoffLimit` 2, `activeDeadlineSeconds` 600, kept for `ttlSecondsAfterFinished` (a day) so a failure stays
visible to `RustryxMigrationFailed`. The Job runs with its own hook copies of the ServiceAccount and the ConfigMap
(weights -10 and -5): on a first install no regular resource exists when a pre-install hook runs. The Deployment
never migrates; a schema older than the binary keeps `/readyz` at 503.

### `monitoring`, `dashboards`

| Key | Default | Meaning |
|---|---|---|
| `monitoring.flavor` | `prometheus` | `prometheus` (ServiceMonitor, PrometheusRule) or `victoriametrics` (VMServiceScrape, VMRule) |
| `monitoring.serviceMonitor.enabled`, `interval`, `scrapeTimeout`, `labels` | off, 30s, 10s | |
| `monitoring.serviceMonitor.bearerTokenSecret` | empty | default is the pods' own Secret and `RUSTRYX_OBSERVABILITY_METRICS_TOKEN` |
| `monitoring.rules.enabled`, `labels`, `alertLabels` | off | extra labels on the rule object and on every alert |
| `monitoring.rules.runbookBaseUrl` | the GitHub runbook | base of every `runbook_url`; the anchor is the lower-cased alert name |
| `monitoring.rules.kubeStateMetrics` | `true` | keep group `rustryx.kube` (migration Job, readiness, restarts, memory) |
| `monitoring.rules.disabled` | `[]` | alert names to drop |
| `dashboards.enabled`, `label`, `labelValue`, `namespace`, `folder` | off, `grafana_dashboard`, `"1"` | ConfigMap for the Grafana sidecar |

How the monitoring assets reach the chart. The sources are `deploy/monitoring/rustryx-alerts.yaml` and
`rustryx-dashboard.json`: plain files usable without Helm (Prometheus, vmalert, Grafana provisioning). Helm can
read only files inside the chart, so `deploy/monitoring/sync-to-chart.sh` copies them to
`deploy/helm/rustryx/files/` (a symlink would break in a GitHub zip download) and `test.sh` fails on drift. The
rule template parses the file, drops disabled alerts, merges `alertLabels`, rewrites the runbook base and
rewrites the default names `rustryx` and `rustryx-migrate` in the kube group to this release. It refuses to render
when the file is invalid or an alert has no `runbook_url`: an empty or partial rule set that looks healthy is the
failure to avoid.

Three decisions worth knowing:

* Prometheus scrapes through a **headless metrics Service with `publishNotReadyAddresses`**. A PostgreSQL outage
  makes every pod NotReady by design; a normal Service would drop them, the targets would vanish and `up == 0`
  would never fire.
* The ServiceMonitor sets `jobLabel: app.kubernetes.io/name`, so `job="rustryx"` whatever the release is called,
  and `honorLabels: false`, so a series that carried a `job` label of its own would become `exported_job` instead of
  overwriting the scrape job. The background job metrics avoid the clash by naming the job in the label `task`, and
  the rules select `task` for job names.
* Rules and panels select `job="rustryx"` on every series: the metric names are generic.

### Single listener and the ops port

Rustryx serves the API, `/healthz`, `/readyz` and `/metrics` on one listener (`server.listen_addr`). The
requirements recommend a separate internal port for probes and metrics (OPS-35); the configuration schema has no
second listener yet, and the chart does not invent a key the schema does not know. `containerPorts.ops` is ready
for it: when set, the chart declares the `ops` container port, points the probes, the metrics Service, the
ServiceMonitor and the NetworkPolicy scraper rule at it, and gives the Service an `ops` port. You must also set the
key that opens that listener under `config.server` (or `config.extra`) once the application has one. Until then,
leave it `null`; the cost is that `/metrics` shares the API port and is guarded by its Bearer token only.

## Render-time validation

All rules are in `templates/_validate.tpl` and run for every render (`templates/validate.yaml` renders nothing
but calls them). Each has a unit test.

| Rule | Why |
|---|---|
| image tag `latest`, malformed digest, missing digest with `requireDigest`, unknown variant, tag inside `repository` | OPS-18 |
| `ingress.enabled` and `gateway.enabled` together without `gateway.allowWithIngress`; Gateway without `parentRefs` | two entry points double the TLS and body-size configuration |
| `config.server.base_url` empty, or not https in production | OIDC redirects and Docker token realms |
| `storage.type: s3` without endpoint, bucket and region | the application requires all three |
| local storage with more than one replica without confirmed ReadWriteMany | OPS-38 |
| production with fewer than two replicas without `allowSingleReplica` | OPS-31 |
| `autoscaling.minReplicas` below `replicaCount`, `maxReplicas` below `minReplicas` | OPS-32 |
| PDB with both fields, none, a `minAvailable` not below the replica floor, or `maxUnavailable: 0` | a PDB that blocks every drain |
| `secrets.entries.databaseUrl` off; production without `internalJwtKey`, `cookieKey`, `bootstrapAdminPassword`, `databaseSealingKey` | the application would not start |
| `/metrics` not public and no `metricsToken` | the application would not start |
| `lock.backend: redis` without `lockRedisUrl` | the application would not start |
| OIDC or LDAP enabled without the required settings or the secret entry | OPS-51 |
| plain `ldap://` without `starttls: true` | README 8.4 |
| credentials in a URL, or any secret key in `config` (including `extra`) | OPS-39 |
| an enabled dependency without an egress peer in the NetworkPolicy | silent timeout |
| `networkPolicy.cilium.enabled` without hosts; subdomain connector without `baseDomain`; custom CA without a source | incomplete configuration |
| a probe with `initialDelaySeconds` next to a startup probe; a probe port that is not the container port; an aggregated health path | OPS-33 |
| an alert without `runbook_url`, or an invalid rule file | OPS-41 |

## Testing the chart

```sh
deploy/helm/test.sh
```

One script runs everything and exits non-zero if anything failed, after running all of it. Needs `helm`, the
`helm-unittest` plugin (`helm plugin install https://github.com/helm-unittest/helm-unittest --verify=false`),
`kubeconform`, `promtool`, `jq` and `python3` with PyYAML. kubeconform downloads the Kubernetes and CRD schemas
(cached); set `SKIP_KUBECONFORM=1` offline. `kube-linter` runs when installed, `MANIFEST_SCANNER` replaces it with
checkov, kubescape or another command that takes a manifest file.

| Stage | What it proves |
|---|---|
| lint and template matrix | 16 value combinations render and lint with `--strict`: S3 and local storage, Ingress and Gateway, subdomain connector, autoscaling on and off, monitoring on and off (both flavors), everything on, `values-test.yaml`, `values-prod.yaml` |
| kubeconform | every rendered resource is valid, strict, against Kubernetes 1.32 and the CRD schemas of ServiceMonitor, VMServiceScrape, PrometheusRule, VMRule, HTTPRoute, ExternalSecret, CiliumNetworkPolicy; a skipped resource fails the stage |
| invariants | `check_rendered.py` on every case: hardening, probes, no `latest`, no Secret, no RBAC, no literal secret, no `VAULT_*`; and every key written to `config.yaml` exists in `config.example.yaml` (an unknown key is a pod that never starts) |
| manifest scanner | kube-linter with the policy in `deploy/helm/kube-linter.yaml` |
| negative cases | 30 inputs that must fail with the stated reason, and the invariant checker is shown to catch five injected violations |
| hygiene | no credential-like literal in values, no `latest`, no em or en dash in owned files, probes outside any `range`, every top-level key documented here |
| monitoring | `promtool check rules` (also on the RENDERED rule), `promtool test rules`, dashboard and catalogue checks, drift between sources and chart copies |
| packaging | `helm package` contains `files/` and excludes `ci/` and `tests/` |
| unit tests | `helm unittest` suites in `deploy/helm/rustryx/tests`, plus the template-level defences run with `--skip-schema-validation` |

CI matrix inputs are `deploy/helm/rustryx/ci/*.yaml`, layered over `base-values.yaml`. To add a case, add a file
and one `add_case` line in `test.sh`. Test names that assert a negative requirement start with `must_not_`, like
the Rust tests (CONTRIBUTING rule 8).

## Open items

* The image tag scheme (`<version>-musl`, `<version>-glibc`) is the chart's assumption of what CI publishes;
  `image.variants` adapts it.
* The container entrypoint is assumed to be the `rustryx` binary with `serve` and `migrate` as arguments.
* The metrics listed as "not yet exported" in [slo.md](slo.md) and the `class` and `cache` labels of the duration
  histogram are needed by some alerts.
* A dedicated ops listener, the Docker subdomain connector and a key ring for the internal JWT are not in the
  configuration schema yet.
