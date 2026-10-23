# Service levels, SLIs and the metric catalogue

This page defines what Rustryx promises, how that is measured and which series the dashboard and the
alerts read. The numbers come from the requirements (SRE-14 to SRE-16 and README 8.2). The rules that
implement them are in `deploy/monitoring/rustryx-alerts.yaml`, the dashboard is
`deploy/monitoring/rustryx-dashboard.json`, and the response to each alert is in
[runbook.md](runbook.md).

## Objectives

| Objective | Target | Window | Gate |
|---|---|---|---|
| Availability | 99.9% of requests without 5xx | rolling 30 days (error budget about 43 minutes) | alert, not a merge gate |
| Metadata latency | p95 below 300 ms | per node, 5 minute window | acceptance gate |
| Time to first byte of a cache-hit download | p95 below 150 ms | per node, 5 minute window | acceptance gate |

The p99 figures of the architecture document are capacity targets for sizing, not gates. They are not
alerted on and not part of the acceptance run (README 8.2).

### Availability SLI

The SLI is the share of requests that did not end in a 5xx:

```
1 - (5xx requests) / (all requests)         over route != /healthz, /readyz, /metrics
```

* **Errors** are 5xx responses and timeouts (a timeout is a 5xx or 504 at the edge of the service).
* **Not errors**: 4xx of every kind, including 401, 403, 404, 409 and 429. A 429 that comes from the
  service's own backpressure is counted separately by `http_shed_total{reason}` and is not an error of
  the SLI, but it is alerted on (`RustryxLoadShedding`) because a service that sheds is a service that is
  close to its limits.
* **Excluded routes**: `/healthz`, `/readyz` and `/metrics`. A kubelet probing a node that is draining
  must not move the SLI, and neither must a scraper.
* The `route` label is the matched route template, never the raw path, so the exclusion is exact.

### Latency SLIs

`http_request_duration_seconds` measures the time until the response head is produced. For a streamed
download that is the time to first byte, which is exactly the SLI of the second objective; the time to
stream the body is not in the histogram and is not part of the objective. Cache-miss downloads through a
proxy repository depend on the upstream and are measured by `upstream_request_duration_seconds`, outside
the objective (SRE-15).

Which requests are "metadata" (REST API, listings, search, `maven-metadata.xml`, npm packuments, pub
package documents) and which are "download, cache hit" cannot be told from the route template alone,
because every format sits under one `/repository/{name}/*` route. See assumption A2 below.

## Recording rules

| Rule | Meaning |
|---|---|
| `rustryx:sli_availability:ratio_rate5m`, `...30m`, `...1h`, `...6h` | availability over the window, per namespace |
| `rustryx:sli_availability:ratio_rate30d` | availability over 30 days (evaluated every 5 minutes) |
| `rustryx:slo_error_budget_remaining:ratio30d` | 1 = full budget left, 0 = spent, negative = SLO missed |
| `rustryx:http_request_duration_seconds:p95_metadata_5m` | p95 of the metadata class |
| `rustryx:http_request_duration_seconds:p95_download_hit_5m` | p95 of cache-hit downloads |
| `rustryx:job_stale_threshold:seconds` | per-job staleness threshold used by `RustryxJobStale` |

The availability rules add `or 0 * ...` to the numerator. Without it a namespace that has traffic but no
5xx series yet returns nothing instead of 1, and the dashboard shows "no data" instead of 100%. A test in
`deploy/monitoring/tests/rustryx-alerts_test.yaml` pins that, and another test pins that a different
service exporting `http_requests_total` in the same namespace does not move the SLI.

## Burn rate alerts

The budget is 0.1% of requests. A burn rate of 1 spends the budget in exactly 30 days.

| Alert | Condition | Severity | Spends |
|---|---|---|---|
| `RustryxErrorBudgetBurnFast` | burn rate above 14.4 on 5 minutes **and** on 1 hour, for 2 minutes | critical | 2% of the monthly budget per hour |
| `RustryxErrorBudgetBurnSlow` | burn rate above 6 on 30 minutes **and** on 6 hours, for 15 minutes | high | 5% of the budget per 6 hours |
| `RustryxHigh5xx` | availability below 98% over 5 minutes, for 10 minutes | high | direct symptom alert |

Two windows per alert: the long window stops a short spike from paging, the short window makes the alert
resolve quickly once the cause is gone.

## Error budget policy

* More than 50% of the monthly budget spent: new feature rollouts to production wait for the cause to be
  understood. Fixes and reverts are always allowed.
* Budget spent: only changes that improve reliability are rolled out until the 30-day SLI recovers.
* Planned maintenance that returns 5xx counts against the budget like an outage. Avoid it with the
  expand/contract migration rule and a rolling update with `maxUnavailable: 0` (see the runbook).

## Metric catalogue

Names are exactly what `crates/rustryx-observability` exports, without a product prefix, as SRE-08
spells them. The names are generic (`http_requests_total` exists in many services), so every selector in
the rules and the dashboard carries `job="rustryx"`. `deploy/monitoring/check_assets.py` checks that, and
it checks that every queried series is listed here and that this page is truthful about what the code
exports today (it reads `names.rs`). A rule that queries a series nobody emits is an alert that cannot
fire; this section makes that visible.

### Exported by the code today

| Series | Type | Labels | Used by |
|---|---|---|---|
| `http_requests_total` | counter | route, method, status | availability SLI, request rate, 5xx share |
| `http_request_duration_seconds` | histogram | route, method | latency panels, route latency (and the class SLIs, see A2) |
| `http_requests_in_flight` | gauge | none | in-flight panel, HPA signal candidate |
| `http_shed_total` | counter | reason | `RustryxLoadShedding` |
| `upstream_requests_total` | counter | repo, status | upstream panels |
| `upstream_request_duration_seconds` | histogram | repo | upstream p95 |
| `upstream_errors_total` | counter | repo, kind | `RustryxUpstreamErrors`, `RustryxUpstreamCircuitOpen` |
| `db_pool_size` | gauge | none | pool panel |
| `db_pool_idle` | gauge | none | `RustryxDbPoolWait` |
| `db_pool_in_use` | gauge | none | pool panel |
| `db_pool_wait_seconds` | histogram | none | pool wait p95 |
| `db_pool_wait_total` | counter | none | `RustryxDbPoolWait` |
| `auth_attempts_total` | counter | method, result | `RustryxAuthFailSpike`, auth panel |
| `background_job_runs_total` | counter | job, result | `RustryxJobFailed`, jobs panel |
| `background_job_last_success_timestamp_seconds` | gauge | job | `RustryxJobStale`, jobs panel |
| `distlock_lost_total` | counter | key_class | `RustryxLockLost` |

Standard process series (`process_cpu_seconds_total`, `process_resident_memory_bytes`,
`process_open_fds`, `process_max_fds`) and the scrape series `up` are not listed above because they are not
produced by `names.rs`.

### Not yet exported by the code

These are required by the requirements (SRE-08, SRE-25, SRE-29) or needed by an alert, and are queried by
the rules and the dashboard, but `names.rs` does not define them yet. Until they exist, the alerts below
stay silent and the panels stay empty. When the code starts exporting one, move it to the table above;
`check_assets.py` fails while a series is on both lists.

| Series | Type | Labels | Needed by |
|---|---|---|---|
| `db_up` | gauge | none | `RustryxDbUnavailable`: 1 while the last readiness `SELECT 1` succeeded, 0 otherwise (the twin of `redis_up`) |
| `redis_up` | gauge | none | `RustryxRedisUnavailable` (SRE-20) |
| `redis_requests_total` | counter | result | Redis panel |
| `blob_store_ops_total` | counter | store, op, result | `RustryxBlobStoreErrors`, blob panels |
| `blob_store_bytes_total` | counter | store, direction | throughput panel |
| `blob_store_free_bytes` | gauge | store | `RustryxDiskLow`, `RustryxDiskCritical` (local store) |
| `blob_store_total_bytes` | gauge | store | disk alerts (SRE-13 uses it, SRE-08 omits it) |
| `background_job_running` | gauge | job | `RustryxJobRunningTooLong`, jobs panel |
| `distlock_acquire_total` | counter | key_class, result | lock panel |
| `ldap_requests_total` | counter | result | `RustryxLdapErrors` |
| `oidc_requests_total` | counter | op, result | `RustryxOidcRefreshFailures` |
| `oidc_jwks_age_seconds` | gauge | none | `RustryxOidcJwksStale` |
| `tokio_busy_ratio` | gauge | none | `RustryxTokioSaturated` |
| `tokio_global_queue_depth` | gauge | none | runtime panel |
| `tokio_blocking_queue_depth` | gauge | none | runtime panel |
| `upstream_circuit_state` | gauge | repo | circuit breaker panel |
| `auth_breakglass_logins_total` | counter | none | `RustryxBreakGlassLogin` (SRE-29 asks for an alert on every break-glass login; `auth_attempts_total` cannot tell the break-glass account from other local users) |

## Assumptions the assets make

| ID | Assumption | If it is wrong |
|---|---|---|
| A1 | The scrape job is named `rustryx` (ServiceMonitor `jobLabel: app.kubernetes.io/name`). | Every rule matches nothing. `RustryxTargetsAbsent` fires after 5 minutes, which is the visible symptom. |
| A2 | `http_request_duration_seconds` carries two extra labels beyond SRE-08: `class` (`metadata`, `download`, `upload`, `api`, `other`) and `cache` (`hit`, `miss`, `none`). | The two class SLI recording rules return nothing, so `RustryxMetadataLatencyP95` and `RustryxDownloadTtfbP95` cannot fire. Availability, burn-rate and route panels are not affected. Alternative: derive the class from the route template once the API crate defines templates that separate the classes. |
| A3 | `background_job_last_success_timestamp_seconds{job}` is initialised to the process start time for every job. | A job that never ran has no series and never goes stale. |
| A4 | The background job metrics label the job name `job`. | Prometheus renames it to `exported_job` on scrape (the rules select `exported_job`). If the code uses another label name, rename it in the rules, the staleness table and the dashboard. |
| A5 | Series carry `namespace` and `pod` from the scrape (ServiceMonitor target labels). | Aggregations by `namespace` collapse into one group; alerts still fire. |
| A6 | `route` of probe and metrics requests is exactly `/healthz`, `/readyz`, `/metrics`. | Probes count in the availability SLI. |
| A7 | One namespace holds one Rustryx release, or the releases share an SLO. | Alerts aggregate by namespace, not by release. |
| A8 | Job names in `rustryx:job_stale_threshold:seconds`: `blob_gc`, `cleanup`, `audit_rotation`. | A job missing from the table is never alerted as stale. Align the table with `rustryx-tasks`. |

## Series that make a scrape worth having

Rustryx has two dependency-shaped failure modes that a plain `up` alert misses, and the chart is built
around them:

* A PostgreSQL outage turns every pod NotReady by design. A normal Service drops NotReady pods from its
  endpoints, so Prometheus would lose the targets and `up == 0` would never fire. The chart therefore
  scrapes through a headless Service with `publishNotReadyAddresses`, and `RustryxTargetsAbsent` covers the
  case where the scrape configuration itself is gone.
* A job that is alive but not working (stalled GC, a lock nobody renews) is invisible to probes. The job
  staleness, failure and running-too-long alerts exist for that (SRE-13).

## Verifying the alerts

SRE-40 requires every critical and high alert to be fired artificially on a stand. The unit tests in
`deploy/monitoring/tests/` prove the expressions on synthetic series (`promtool test rules`); a stand test
proves the wiring (labels, routing, runbook link). Suggested triggers:

| Alert | Trigger on a stand |
|---|---|
| `RustryxDown` | scale the Deployment to 0 for 3 minutes |
| `RustryxReplicaDown` | block the scrape of one pod with a NetworkPolicy for 6 minutes |
| `RustryxDbUnavailable` | stop PostgreSQL (or block egress to it) for 90 seconds; pods go NotReady, restarts must stay 0 |
| `RustryxHigh5xx` and the burn-rate alerts | point a test proxy repository at a closed port and request it in a loop |
| `RustryxJobStale` | lower the threshold in a copy of the rule file, or stop the singleton job |
| `RustryxLockLost` | kill the lock holder's database session (`pg_terminate_backend`) while a job runs |
| `RustryxMigrationFailed` | run an upgrade with a deliberately failing migration on a copy |
