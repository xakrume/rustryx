# Operations runbook

Audience: the person on call and the person doing a planned change. Every alert of
`deploy/monitoring/rustryx-alerts.yaml` links to a section of this page whose heading is the alert name.
Chart values are explained in [helm.md](helm.md), service levels in [slo.md](slo.md), configuration keys
in [config.md](config.md).

Conventions used below:

```sh
export NS=rustryx                 # namespace of the release
export REL=rustryx                # name of the Deployment: the release name when it contains "rustryx",
                                  # otherwise <release>-rustryx (the chart's fullname)
kubectl config current-context    # say which cluster you are on before you change anything
```

The image is distroless: there is no shell and no `curl` in the pod. Use `kubectl port-forward` from your
machine, `kubectl logs`, and `kubectl debug` with an ephemeral container when you need a shell next to the
process.

## How Rustryx fails

Nodes are identical and stateless. State lives in PostgreSQL (metadata, RBAC, audit), the blob store (S3 or
one shared volume) and, optionally, Redis (locks only when `lock.backend` is `redis`). The behaviour below
is by design (README 8.2, 8.3, 8.4); do not "fix" it by restarting pods.

| Dependency down | `/healthz` | `/readyz` | What still works | What breaks |
|---|---|---|---|---|
| PostgreSQL | 200 | 503 after 1 s, all pods NotReady, **not restarted** | nothing that needs metadata | everything; pods return by themselves when PostgreSQL returns |
| Redis | 200 | 200 | requests, singleton jobs if `lock.backend` is `postgres` | with `lock.backend: redis`, singleton jobs do not start (fail closed) |
| S3 / blob store | 200 | 200 | metadata, cached reads, logins | uploads and reads of blobs that are not yet cached fail |
| Keycloak | 200 | 200 | local users, `ryx_*` tokens, Keycloak bearer tokens until `exp` (checked against the last JWKS held in memory) | new interactive OIDC logins get 503 |
| LDAP | 200 | 200 | everyone but LDAP users | LDAP users lose roles after the 5 minute group cache ends |
| Upstream registry | 200 | 200 | hosted repos, cached proxy content (stale served with `X-Rustryx-Cache: stale`) | proxy cache misses return 502 or 504; groups skip the failing member |
| Schema older than the binary | 200 | 503 | nothing new | the migration did not run: see [Migrating](#migrating-the-database-schema) |

Readiness depends on PostgreSQL only. The cascade risk is accepted: without the database a node cannot
serve one metadata request, so leaving the balancer is correct, and because liveness touches no
dependency the pods are not restarted and come back together.

## Routine operations

### Verifying probes on a live pod

Before every release, on the image that will ship (SRE-05). A 404 here means a guaranteed restart loop.

```sh
kubectl -n $NS port-forward deploy/$REL 8081:8081 &
curl -fsS -w ' %{http_code}\n' localhost:8081/healthz     # {"status":"ok"} 200
curl -fsS -w ' %{http_code}\n' localhost:8081/readyz      # 200 with the list of checks
kill %1
```

Replace 8081 with `containerPorts.http` (or `containerPorts.ops` when a dedicated listener is configured).
Measure the cold start from the log line `started in N seconds` and check that the startup probe budget
(`periodSeconds x failureThreshold`, 150 s by default) is at least three times that.

### Scaling

* Replicas: set `replicaCount` (two is the floor in production) or let the HPA run
  (`autoscaling.minReplicas` to `maxReplicas`). Capacity grows with nodes, but PostgreSQL does not.
* Connection budget: `max replicas x config.database.pool_size` plus a reserve for migrations, admin and
  other clients must stay below the PostgreSQL `max_connections`. With a transaction-mode pooler in front,
  advisory locks and `LISTEN/NOTIFY` need a direct session: give the app `RUSTRYX_DATABASE_DIRECT_URL`
  (`secrets.entries.databaseDirectUrl`).
* Check the effect of scaling on `db_pool_idle` and `RustryxDbPoolWait` before adding more replicas.
* Scale-down is deliberately slow (5 minute stabilisation) so that a node is not removed in the middle of
  an upload. A node that is removed drains for `shutdown.timeoutSeconds` first.

### Rolling upgrade (expand/contract)

The rule that makes zero-downtime upgrades possible (OPS-72): **release N never removes or renames
anything release N-1 uses.** Destructive steps ship in N+1, after N is fully rolled out. Schema N-1 is
accepted by binary N; a schema older than the binary keeps `/readyz` at 503.

1. Read the release notes for the list of migrations and breaking changes.
2. `helm upgrade` (or merge the manifest change for GitOps). The migration Job runs first as a hook; the
   Deployment starts rolling only after it succeeded.
3. The rollout adds one new pod at a time (`maxSurge: 1`) and removes an old one only after the new one is
   Ready (`maxUnavailable: 0`). An old pod stops being routed to first (readiness 503 and a 10 s preStop
   pause), then drains in-flight requests, then exits 0.
4. Watch `kubectl -n $NS rollout status deploy/$REL`, the 5xx share, and `RustryxContainerRestarts`.
   During the rollout the 5xx count should be zero; a request cut by SIGTERM is a bug, not an expectation.

### Migrating the database schema

Schema changes happen only through `rustryx migrate` and never from `serve`. The command takes a
PostgreSQL advisory lock on a direct (non-pooled) connection, so two concurrent runs cannot corrupt the
schema: the second waits or exits cleanly.

* The chart runs it as a Helm hook Job (`pre-install,pre-upgrade`; PreSync with ArgoCD) named
  `<fullname>-migrate`. It uses the same image, config and secrets as the pods, has `backoffLimit` 2 and a
  600 s deadline, and is kept for a day after it finishes.
* Read its result: `kubectl -n $NS logs job/$REL-migrate`.
* If it failed, the upgrade stopped before any pod changed: old pods keep serving. Fix the cause, then run
  the upgrade again; hook resources are recreated (`before-hook-creation`). Alert: `RustryxMigrationFailed`.
* Running it by hand (for example to preview the target version on a restored copy):
  `kubectl -n $NS create job --from=job/$REL-migrate manual-migrate`, or run `rustryx migrate` with the same
  environment from a debug pod. Always against a copy first when the release notes mention a data migration.
* First install with `externalSecret.enabled`: the hook runs before regular resources exist. Either create
  the Secret first, or set `externalSecret.hook=true` with `creationPolicy: Orphan` (see helm.md).

### Rolling back

Rolling back N to N-1 needs no database restore, because N never removed what N-1 uses.

* Helm: `helm rollback $REL <revision>` (the migration hook runs again with the old binary, which finds
  schema N, accepts it as newer-but-compatible, and does nothing).
* GitOps: revert the commit or merge request in the manifest repository. Do not rewrite history.
* Do **not** run a migration "down". There are none: migrations only go forward.
* If N contained a destructive step by mistake, it is not a rollback case: restore from backup, see below.

### Backup and restore

Backups that were never restored are not backups (SRE-38). Targets, to be agreed per site: RPO 15 minutes,
RTO 2 hours.

What to back up:

| What | How | Notes |
|---|---|---|
| PostgreSQL | base backup plus continuous WAL archiving (pgBackRest, WAL-G, or the managed service's PITR) | holds metadata, RBAC, audit and the blob references |
| Blobs | S3 bucket versioning or bucket replication; for a local store, volume snapshots | a blob is only deleted by GC after the age threshold, which gives versioning a safety margin |
| Secrets and keys | the secret manager behind `secrets.existingSecret` | losing `RUSTRYX_AUTH_INTERNAL_JWT_KEY` only logs UI users out; losing the database password is a rotation |
| Config | git (values files) | nothing in the cluster is the only copy |

Order matters: **the blob store must not be older than the database.** A database restored to time T needs
every blob that existed at T. A blob store that is newer than the database only holds extra blobs that nothing
references, and GC removes them after `gc.min_age_secs`. The opposite leaves references that point at nothing.
Therefore restore the database, and restore the bucket only if it was lost or damaged, and then to a point not
earlier than the database.

Restore procedure:

1. Stop writes: `kubectl -n $NS scale deploy/$REL --replicas=0` (also pause the HPA by setting `autoscaling.enabled=false`
   or the HPA will scale it back up).
2. Restore PostgreSQL to the chosen point in time into a new instance, or over the old one.
3. Point `RUSTRYX_DATABASE_URL` (and the direct URL) at it if the address changed; restart is by rollout.
4. If the bucket was restored, confirm its newest object is not older than the database point.
5. Run the migration Job for the version you are about to start (`helm upgrade` does it), so the schema matches
   the binary.
6. Scale up. `/readyz` goes 200 when PostgreSQL answers and the schema matches.
7. Verify (automate this monthly): artifact count against the compat v1 listing, and the checksum of a sample of at
   least 1% of blobs against the checksums the API reports.
8. Record the result. Run GC dry-run reporting before enabling destructive GC on a restored system.

### Rotating secrets and keys

Secrets reach the pods once, at startup: in env mode through `envFrom`, in file mode as files read at startup.
Neither mode hot-reloads, so **every rotation ends with a rolling restart**: `kubectl -n $NS rollout restart
deploy/$REL`. Change the Secret (or the secret store entry the ExternalSecret reads) first, wait for it to sync,
then restart. The migration Job reads the same Secret on the next upgrade.

| Secret (env name) | Procedure | Impact |
|---|---|---|
| Database password (`RUSTRYX_DATABASE_URL`, `RUSTRYX_DATABASE_DIRECT_URL`) | Use two roles to avoid a window of failed connections: create `rustryx_b` with the same grants, put its URL into the Secret, roll the pods, then drop the old role. Changing one role's password in place makes every pool reconnect with the old DSN fail until the roll finishes. | none with two roles |
| S3 keys (`RUSTRYX_BLOB_S3_ACCESS_KEY`, `RUSTRYX_BLOB_S3_SECRET_KEY`) | Create a second key pair at the provider, update the Secret, roll the pods, then revoke the old pair. | uploads in flight at revocation fail and are retried by clients |
| OIDC client secret (`RUSTRYX_AUTH_OIDC_CLIENT_SECRET`) | Regenerate in Keycloak, update the Secret, roll the pods. Bearer validation uses only the JWKS, so API clients are unaffected. | interactive logins fail between the Keycloak change and the pods' restart; do it quickly |
| LDAP bind password (`RUSTRYX_AUTH_LDAP_BIND_PASSWORD`) | Change in the directory, update the Secret, roll. | LDAP lookups fail between the two steps; group cache (5 minutes) covers established sessions |
| Metrics token (`RUSTRYX_OBSERVABILITY_METRICS_TOKEN`) | Update the Secret, roll the pods. The chart's ServiceMonitor reads the same Secret, so the scraper follows once the operator reloads it. | a scrape gap of a few intervals; `RustryxReplicaDown` may flap |
| Redis URL (`RUSTRYX_LOCK_REDIS_URL`) | Update the Secret, roll. Singleton jobs pause while pods hold different URLs only if the new URL points at a different Redis; never run two backends. | brief pause of singleton jobs |
| Bootstrap admin password (`RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD`) | Used only to create the break-glass account on an empty database. Change the account's password in the application, then remove the entry from the Secret (`secrets.entries.bootstrapAdminPassword=false`). Rotate the password stored for emergencies on the agreed schedule. | none |
| Cookie key (`RUSTRYX_AUTH_COOKIE_KEY`) | Generate `openssl rand -base64 32`, update the Secret, roll. | the sealed `state` and `nonce` cookie of a login in progress becomes invalid: that user retries. Existing sessions are not stored in this cookie. |
| Internal JWT key (`RUSTRYX_AUTH_INTERNAL_JWT_KEY`) | See below. | every UI session is invalidated |

**Internal JWT key.** The key is a PEM Ed25519 private key that signs the internal JWTs given to the UI after
login. It must be identical on every node. The configuration schema carries exactly one key today. Rotating it
means: generate a new key, update the Secret, roll the pods. Pods that already restarted sign with the new key
and reject tokens signed with the old one, so during the rollout a UI request can land on a pod that disagrees
with the one that issued the token and gets 401; users log in again. `ryx_*` API tokens and Keycloak bearer
tokens do not use this key and are unaffected. The architecture describes key ids and a ring with overlap
(the new key signs, the old key still verifies) for rotation without a forced re-login. That ring is not in the
configuration schema yet; use it as soon as it is, by adding the new key first and removing the old one after
the longest session lifetime (8 hours absolute). Until then, rotate in a quiet window.

**JWKS issues** (Keycloak keys). The service refreshes the Keycloak key set at most once per 30 seconds per
issuer, and only for an unknown `kid`; a key that disappears from the latest successful fetch is no longer
accepted; keys are accepted only from HTTPS with a verified certificate.

* Symptom after a Keycloak key rotation: bearer tokens signed with the new key get 401 for up to 30 seconds,
  then work. Nothing to do.
* Symptom: all bearer tokens get 401 and `oidc_jwks_age_seconds` grows. Keycloak or the path to it is down, or
  its certificate is no longer trusted (expired, new CA). Fix the path or the trust (`tls.customCA`); do not
  disable verification. Tokens that are still valid keep working against the last key set in memory.

### GC and cleanup runs

Blob GC removes only blobs with no reference and older than `gc.min_age_secs` (24 hours by default, which must
exceed `limits.request_timeout_secs` so that an upload in progress is never collected). Cleanup policies delete
by age, version count and non-use, with a dry run before a real run, and never touch release repositories.
Both are singleton jobs that run under a distributed lock, so only one node executes a run at a time.

* Schedule and limits: `config.gc.*` (see config.md). The chart does not own them.
* Watch: `background_job_last_success_timestamp_seconds{exported_job="blob_gc"}`, `background_job_runs_total`,
  the logs of the node holding the lock.
* A run that fails leaves state the next run picks up; jobs are idempotent. Alerts: `RustryxJobFailed`,
  `RustryxJobStale`.
* To run sooner after a disk-low event, lower `gc.interval_secs` through values (`config.gc.interval_secs`) and
  roll; restore the value afterwards. If your build exposes an administrative trigger, prefer it.
* Always take a dry run report before enabling or tightening a cleanup policy in production.

### Lock backend notes

* `lock.backend` is `postgres` (default) or `redis`. It is fixed for the life of the process. There is no
  automatic fallback: two backends at once would give two holders. To change it, change the value and roll.
* `postgres`: a session-level advisory lock on a dedicated direct connection, released when the connection ends.
  A pooler in transaction mode breaks it; use `RUSTRYX_DATABASE_DIRECT_URL`.
* `redis`: `SET NX PX` with an owner token, TTL 30 s, renewed every TTL/3. If Redis is unreachable, requests are
  served and singleton jobs do not start.
* A holder that loses its lock stops writing (`distlock_lost_total`); the job continues from stored progress on
  the next run.
* Releasing a lock held by a dead node: the lock expires after the TTL on its own (30 s plus the renewal gap).
  Only when you must not wait: for `postgres`, find the holder with
  `SELECT pid, application_name FROM pg_stat_activity WHERE pid IN (SELECT pid FROM pg_locks WHERE locktype = 'advisory');`
  and end that session with `SELECT pg_terminate_backend(<pid>);` after confirming the node is gone. For `redis`,
  list keys with `redis-cli --scan --pattern 'rustryx:lock:*'` and delete the one of the job concerned, again only
  after confirming that the holder is gone. A lock held by a live node must not be forced.

## Dependency outages

### Keycloak outage

* New interactive OIDC logins get 503 with a clear message. Pods stay Ready.
* Local users, `ryx_*` tokens and the break-glass account work. CI that uses `ryx_*` tokens keeps working.
* Keycloak bearer tokens that have not expired keep validating (signature, `iss`, audience and expiry are
  checked on every request) against the last key set in memory. Access tokens live 5 minutes, so CI that fetches
  fresh tokens from Keycloak stops within minutes.
* Action: restore Keycloak. If admin access is needed meanwhile, use the break-glass account (every login with it
  is audited and alerts, `RustryxBreakGlassLogin`).

### LDAP outage

* LDAP users keep their confirmed group membership for at most 5 minutes after the last successful lookup, then
  their LDAP-derived roles are dropped and requests get 403. An error or timeout in a group search never means an
  empty list of groups.
* Local users, tokens and OIDC users are unaffected.
* Action: restore the directory or the network path (`networkPolicy.egress.ldap`). Check the certificate:
  LDAP is LDAPS or StartTLS with hostname verification, there is no plaintext fallback.

### Redis outage

* With `lock.backend: postgres` Redis is not a dependency of anything that matters; alert `RustryxRedisUnavailable`
  can be disabled (`monitoring.rules.disabled`).
* With `lock.backend: redis`, singleton jobs (cleanup, GC) do not start until Redis is back. Requests are served.
* Action: restore Redis. Do not switch the backend while it is down unless you are prepared to roll all pods
  and accept that no job runs during the roll.

### S3 or blob store outage

* Pods stay Ready. Uploads fail, reads of blobs not in a cache fail, metadata keeps working.
* Check the endpoint, credentials, clock skew (S3 signatures), and `networkPolicy.egress.s3`. Alert:
  `RustryxBlobStoreErrors`.
* A local store that fills up rejects writes with 507 below 10% free space and keeps serving reads and deletes.

### Upstream outage

* The circuit breaker per proxy repository opens above 50% errors; requests stop going to the upstream and stale
  cache is served (`Warning: 110`, `X-Rustryx-Cache: stale`) up to the configured maximum stale age. A miss
  returns 502 or 504.
  Groups skip the failing member.
* Alerts: `RustryxUpstreamErrors`, `RustryxUpstreamCircuitOpen`.
* If one upstream is down for long and it blocks a group, temporarily remove it from the group or disable the
  repository rather than weakening limits for every repository.

### PostgreSQL outage

* All pods become NotReady within seconds and **are not restarted** (`restartCount` must not rise). When the
  database returns, pods become Ready again without action.
* Do not delete pods and do not scale to zero to "reset" them: it only lengthens the recovery.
* Diagnose with `kubectl -n $NS get pods` (READY 0/1, RESTARTS unchanged), `curl /readyz` through port-forward,
  and the database side.

## Alert runbooks

Each section follows the same order: symptoms, diagnosis, action, rollback or escalation. Metric names are in
[slo.md](slo.md). Run the PromQL in Prometheus, Grafana Explore or `vmui`.

### RustryxDown

**Symptoms.** Every scrape target of job `rustryx` reports `up == 0`. The scrape goes through the headless
metrics Service that keeps NotReady pods, so this is not a PostgreSQL outage: the pods are gone, crash-looping
or unreachable.

**Diagnose.**
```sh
kubectl -n $NS get deploy,pods -l app.kubernetes.io/name=rustryx
kubectl -n $NS describe pod <pod>          # events: image pull, OOMKilled, probe failures
kubectl -n $NS logs <pod> --previous       # config errors list all problems, exit code 78
```
A config problem exits with code 78 and prints every error at once. A NetworkPolicy that blocks the scraper
looks the same from Prometheus: check `networkPolicy.ingress.metricsScrapers`.

**Act.** Fix the cause (config, secret, image). If a release caused it, roll back (see Rolling back).
**Escalate** after 15 minutes without a Ready pod.

### RustryxTargetsAbsent

**Symptoms.** The series `up{job="rustryx"}` does not exist at all.

**Diagnose.** The ServiceMonitor or VMServiceScrape is missing or does not match the Prometheus selector
(`monitoring.serviceMonitor.labels`), the metrics Service has no endpoints, or the job label changed.
```sh
kubectl -n $NS get servicemonitor,vmservicescrape
kubectl -n $NS get endpoints $REL-metrics
```
**Act.** Fix the labels or the selector. This alert is the safety net that makes every other alert trustworthy:
without targets, rules evaluate to nothing and look green.

### RustryxReplicaDown

**Symptoms.** One pod is not scraped; others may be fine; capacity is reduced.

**Diagnose.** `kubectl -n $NS get pods -o wide`; compare the pod named in the alert with the others (node,
restarts). Check whether only that pod is blocked by a network problem on its node.

**Act.** If the pod is unhealthy, delete it so it is rescheduled. If the node is the problem, cordon and drain it
(the PDB allows one pod to be unavailable). **Escalate** if several pods on different nodes report it.

### RustryxHigh5xx

**Symptoms.** More than 2% of requests fail with 5xx for 10 minutes.

**Diagnose.**
```promql
sum by (route, status) (rate(http_requests_total{job="rustryx",status=~"5.."}[5m]))
```
Which route and status? 503 on everything with `/readyz` 503: PostgreSQL (see RustryxDbUnavailable). 503 with
`Retry-After` on uploads: pool or permits exhausted (RustryxDbPoolWait, RustryxLoadShedding). 502 or 504 on proxy
repositories: upstream (RustryxUpstreamErrors). 500 after a rollout: roll back.

**Act.** Remove the cause; roll back a bad release. **Rollback.** `helm rollback` is safe (expand/contract).

### RustryxErrorBudgetBurnFast

**Symptoms.** The 30 day error budget burns 14.4 times too fast: about 2% of the month per hour.

**Diagnose and act.** Same as RustryxHigh5xx, with urgency: this pages. Look at what changed in the last hours
(release, secret rotation, dependency maintenance). If a release is the cause, roll back first and investigate
afterwards.

**Escalate** immediately if the cause is not understood within 15 minutes.

### RustryxErrorBudgetBurnSlow

**Symptoms.** The budget burns 6 times too fast over 6 hours. A slow leak rather than an outage.

**Diagnose.** Look for a steady low-level failure: a single route, a single proxy repository, one bad node.
```promql
topk(5, sum by (route) (increase(http_requests_total{job="rustryx",status=~"5.."}[6h])))
```
**Act.** Fix during working hours; it becomes RustryxErrorBudgetBurnFast if ignored.

### RustryxMetadataLatencyP95

**Symptoms.** p95 of metadata requests is above 300 ms for 15 minutes.

**Diagnose.** Is the database slow or its pool exhausted (`db_pool_wait_seconds`, `db_pool_idle`)? Is CPU saturated
(`RustryxTokioSaturated`)? Did a large group repository start merging big indexes? Compare with the same hour
yesterday.

**Act.** Add replicas if CPU bound, tune the pool or the database if it waits there, look at slow statements in
PostgreSQL. This alert needs the `class` label on the duration histogram (slo.md, A2); if it never fires, check
that first.

### RustryxDownloadTtfbP95

**Symptoms.** p95 time to first byte of cache-hit downloads is above 150 ms.

**Diagnose.** Blob store latency (`blob_store_ops_total`, S3 provider status, network), node CPU, and whether the
"cache hit" class includes slow cold blobs.

**Act.** Fix the blob store path; scale out; check that the pod has the memory for its working set.

### RustryxLoadShedding

**Symptoms.** The service rejects requests with 429 or 503 to protect itself (`http_shed_total{reason}`).

**Diagnose.** The `reason` label says which limit: concurrency of uploads or downloads, blocking-task permits,
rate limit. Compare with the limits in `config.limits`.

**Act.** If the load is legitimate, scale out and review the limits; if it is one client, look at the audit log
and the rate limit. Do not raise limits blindly: they protect the memory limit of the pod.

### RustryxTokioSaturated

**Symptoms.** The async runtime of a pod is busy more than 90% of the time for 15 minutes.

**Diagnose.** CPU request and throttling (`kubectl top`, container CPU throttling), and whether work that must be
bounded (hashing, archive parsing) runs in the wrong place.

**Act.** Add CPU or replicas. Report anything that blocks a worker thread: that is a defect (CONTRIBUTING rule 1).

### RustryxDbUnavailable

**Symptoms.** `db_up == 0` on every node for 1 minute. All pods are NotReady and not restarting. The service
serves nothing that needs metadata.

**Diagnose.**
```sh
kubectl -n $NS get pods                         # READY 0/1, RESTARTS unchanged
kubectl -n $NS logs deploy/$REL | tail         # connect or timeout errors, never the DSN
```
Check the database: is it up, is `max_connections` exhausted, did a failover happen, is the network path or
`networkPolicy.egress.postgresql` blocking it, did the password change?

**Act.** Restore the database. Do not restart pods: they come back by themselves. After a failover, confirm the
service points at the new primary and that the pooler (if any) is in the right mode for the direct URL.

**Escalate** to the database owner at once; this is a full outage.

### RustryxDbPoolWait

**Symptoms.** Requests waited for a connection and no idle connection was left on a pod.

**Diagnose.**
```promql
max by (pod) (db_pool_in_use{job="rustryx"}) / max by (pod) (db_pool_size{job="rustryx"})
```
A slow query, a transaction held open, a burst, or a pool smaller than the concurrency.

**Act.** Find the slow statement in PostgreSQL (`pg_stat_activity`). Raise `config.database.pool_size` only if the
server has headroom: replicas times pool size must stay below `max_connections`. Requests that cannot get a
connection within 5 seconds get 503 with `Retry-After`, they do not hang.

### RustryxRedisUnavailable

**Symptoms.** `redis_up == 0` for 5 minutes.

**Diagnose and act.** See Redis outage above. Check `networkPolicy.egress.redis` and the Redis URL secret. Remember
that Redis never influences `/readyz`.

### RustryxBlobStoreErrors

**Symptoms.** Blob operations fail (`blob_store_ops_total{result="error"}`).

**Diagnose.** Which store and operation. For S3: endpoint reachability, credentials (a rotated key that was not
rolled out), clock skew, bucket policy, the provider's status. For a local store: the volume, permissions
(`fsGroup`), disk.

**Act.** Fix the cause. Uploads that failed are retried by clients; a partial upload leaves a temporary blob that
GC removes after its age threshold.

### RustryxDiskLow

**Symptoms.** A local blob store has less than 15% free space. Writes are rejected with 507 below 10%.

**Diagnose.** `blob_store_free_bytes / blob_store_total_bytes`. What grew: a repository, a cleanup policy that did
not run, abandoned uploads.

**Act.** Run cleanup and GC (see GC and cleanup runs), then grow the volume (`storage.local.size` and the storage
class permitting expansion). A local store in HA needs ReadWriteMany; S3 does not have this problem.

### RustryxDiskCritical

**Symptoms.** Less than 5% free. Writes are rejected; reads and deletes work.

**Act.** Free space first: delete what can be deleted through the API, run GC. Then expand the volume. The pod
stays Ready (readiness does not depend on disk space except when there is none at all).

### RustryxUpstreamErrors

**Symptoms.** More than 20 upstream errors in 10 minutes for a proxy repository.

**Diagnose.**
```promql
sum by (repo, kind) (increase(upstream_errors_total{job="rustryx"}[30m]))
```
`timeout` or `connect`: upstream or the network path (`networkPolicy.egress.upstream`, DNS). `5xx`: upstream is
unhealthy. `circuit_open`: see RustryxUpstreamCircuitOpen.

**Act.** Check the upstream's status page; verify egress. Stale content is being served meanwhile. If a group is
affected, the failing member is skipped. With Cilium FQDN policy, check the shared-IP trap in helm.md.

### RustryxUpstreamCircuitOpen

**Symptoms.** The breaker of a proxy repository rejected requests: it is open and no request goes upstream.

**Diagnose.** Same as RustryxUpstreamErrors. The breaker half-opens after 30 seconds and lets one probe through.

**Act.** Fix the upstream path. Do not disable the breaker. If the repository must stay unavailable for long,
disable it or remove it from groups temporarily so clients stop waiting for it.

### RustryxJobStale

**Symptoms.** A background job did not succeed within its allowed interval (`rustryx:job_stale_threshold:seconds`):
the process is alive but the job is not working.

**Diagnose.**
```promql
time() - max by (exported_job) (background_job_last_success_timestamp_seconds{job="rustryx"})
```
Is a lock held by a dead node (wait for the TTL, or see Lock backend notes)? Is the lock backend down (Redis)? Is
the job failing every time (RustryxJobFailed) or running forever (RustryxJobRunningTooLong)? Are the logs of the
node that should hold the lock showing why it does not start?

**Act.** Remove the cause. If a stuck holder has to be released, follow the lock procedure above. Run the job again
and confirm `background_job_last_success_timestamp_seconds` moves.

### RustryxJobFailed

**Symptoms.** A job finished with `result="error"` within the last hour.

**Diagnose.** The log of the run: the error names the step. Jobs are idempotent and keep progress in PostgreSQL, so
the next run continues.

**Act.** Fix the cause (blob store, database, permissions). If it repeats every run, treat it as RustryxJobStale in
the making.

### RustryxJobRunningTooLong

**Symptoms.** `background_job_running == 1` for more than 6 hours.

**Diagnose.** A huge GC or cleanup is legitimate; a hung job is not. Does it still write (blob store operations,
progress rows)? Is the lock being renewed (no `distlock_lost_total`)?

**Act.** Wait if it makes progress. If it is hung, restart the pod that holds the lock: the job releases its lock
on shutdown, and the next run resumes. Report a hung job: it is a defect.

### RustryxLockLost

**Symptoms.** A job lost its distributed lock while holding it and stopped writing as designed.

**Diagnose.** Causes: a slow or partitioned lock backend, a node pause (CPU starvation, node pressure), a database
failover (for the postgres backend the session ended). Check whether two nodes could have worked at once: the job
must not have written after the loss.

**Act.** Fix the cause of the stall. Verify the next run completes (`background_job_last_success_timestamp_seconds`).
**Escalate** if the loss repeats.

### RustryxOidcJwksStale

**Symptoms.** The cached Keycloak JWKS is older than one hour.

**Diagnose.** The refresh fails. Look at RustryxOidcRefreshFailures and the logs: Keycloak down, DNS, egress, a
certificate that does not validate. Tokens signed by existing keys keep working; tokens signed by a rotated key
will be rejected until the refresh works.

**Act.** Restore reachability or trust. Do not weaken TLS verification.

### RustryxOidcRefreshFailures

**Symptoms.** Fetching discovery or JWKS from Keycloak fails repeatedly.

**Diagnose.**
```promql
sum by (op, result) (increase(oidc_requests_total{job="rustryx"}[30m]))
```
From a debug pod in the namespace, test the issuer URL and its certificate. Common causes: Keycloak outage,
renewed certificate from a CA the pods do not trust (`tls.customCA`), a changed hostname (`public_issuer_url`
versus `issuer`), an egress policy.

**Act.** Fix the cause. Startup never blocks on OIDC discovery, so pods stay Ready meanwhile.

### RustryxLdapErrors

**Symptoms.** LDAP requests fail or time out.

**Diagnose.** Reachability and certificate of the directory (LDAPS or StartTLS with hostname verification), bind
account status, response time. Remember the 5 minute group cache.

**Act.** Fix the directory or the path. LDAP users lose access when the cache ends; others are unaffected.

### RustryxAuthFailSpike

**Symptoms.** More than 100 denied authentications in 10 minutes for one method.

**Diagnose.** Audit log by source address: credential stuffing from outside, or a CI job with an expired token
retrying in a loop. The login guard counts failures in PostgreSQL per (account, address) and per account and delays
after five failures.

**Act.** Block the source at the edge, revoke a leaked token, fix the CI credential. Local and LDAP logins fail
closed if the failure counter store is unavailable.

### RustryxBreakGlassLogin

**Symptoms.** The break-glass administrator logged in. The audit log has an entry with level warning.

**Diagnose.** Was this expected (an announced emergency)? Who and why: ask, and read the audit entries that follow.

**Act.** If unexpected, treat as a security incident: rotate the break-glass password, review the audit trail,
check who has access to the Secret. After every legitimate use, rotate the password.

### RustryxMigrationFailed

**Symptoms.** The migration Job failed. The upgrade stopped before any pod changed; old pods keep serving.

**Diagnose.** `kubectl -n $NS logs job/$REL-migrate`. Typical causes: a migration error, the database user lacks
privileges, a pooler in transaction mode on the direct URL (advisory lock), a lock held by another migration.

**Act.** Fix and run the upgrade again. If a data migration failed halfway, consult the release notes before
retrying; migrations are written to be idempotent. **Rollback.** Nothing was rolled out; revert the manifest change
if the release must be abandoned.

### RustryxNoReadyReplicas

**Symptoms.** No pod is Ready: the Service has no endpoints.

**Diagnose.** Read `/readyz` through port-forward (it lists the checks, without DSNs or hosts). PostgreSQL
unreachable: see RustryxDbUnavailable. Schema older than the binary: the migration Job did not run or failed (see
Migrating). Startup stuck: check the startup probe budget and the cold start time.

**Act.** Restore the failing check. A rollout that stalled with `maxUnavailable: 0` keeps the old pods, so a
total absence of Ready pods usually means a dependency, not the rollout.

### RustryxContainerRestarts

**Symptoms.** The container restarted. Probes are built so that a dependency outage never restarts a pod; a restart
is a crash, an OOM kill or a failing liveness probe.

**Diagnose.**
```sh
kubectl -n $NS describe pod <pod> | sed -n '/Last State/,/Ready/p'
kubectl -n $NS logs <pod> --previous
```
`OOMKilled`: see RustryxMemoryNearLimit. Exit code 78: configuration error, the log lists every problem. A panic is
caught by the panic layer and logged; an exit with a signal is the kernel or the node.

**Act.** Fix the cause. Never raise `failureThreshold` of the liveness probe to hide restarts.

### RustryxMemoryNearLimit

**Symptoms.** A pod uses more than 90% of its memory limit.

**Diagnose.** Uploads are streamed, so memory is bounded by buffers and concurrency, not by artifact size. Growth
that follows time and not traffic is a leak. Check concurrent uploads (S3 multipart parts are 16 MiB each by
default, `blob.s3_part_size_mb`), `limits.max_in_flight_requests`, and `process_resident_memory_bytes`.

**Act.** Lower concurrency or the part size, raise the limit based on a load test (not by guessing), and report a
leak with a heap profile. An OOM kill will follow if ignored.
