# Server

`rustryx` is the single binary of the project (`crates/rustryx-server`). This page
describes how it starts, serves and stops, what the HTTP layer
(`crates/rustryx-api`) guarantees to every route, and, at the end, exactly what
still has to be wired into the composition root. Configuration is described in
[config.md](config.md).

## Commands

| Command | What it does | Exit codes |
|---|---|---|
| `rustryx serve` | Runs the server. Never changes the database schema. | 0 after a graceful stop, 78 invalid configuration, 1 any other failure (for example the port is taken) |
| `rustryx migrate` | Applies database migrations under an advisory lock. The only place the schema changes. Today it fails with a message that says it is not wired. | 0, 78, 1 |
| `rustryx print-config [--no-validate]` | Prints the effective configuration as YAML, secrets as `[REDACTED]`. `--no-validate` skips the semantic rules, not the structural checks. | 0, 78 |

`--config <path>` (short `-c`) works with every command and wins over
`RUSTRYX_CONFIG`. A usage error exits with 2. Configuration problems are printed to
standard error, all of them at once, before anything else happens.

## Startup order

Each step must succeed before the next one runs. A failure ends the process; no
step blocks waiting for an external system.

1. The configuration is loaded and validated (before any thread, log or port).
2. The multi-thread tokio runtime is built (up to 64 blocking threads).
3. Logging is initialised and, only when `observability.otlp_endpoint` is set, the
   OTLP exporter (gRPC needs the runtime, hence this order). The Prometheus
   recorder is created and installed as the process-wide recorder.
4. SIGTERM and SIGINT handlers are installed. A node that cannot receive its stop
   signal fails here instead of being unstoppable later.
5. `wiring::assemble` builds the components. It must not run migrations and must
   not wait for Keycloak, LDAP, Redis or S3: those are retried in the background.
6. The listener is bound, readiness may report "listening", and the node logs
   `listening` with the bound address.

## Shutdown order

On SIGTERM or SIGINT:

1. Readiness flips to 503 (`/readyz` shows `draining: fail`). `/healthz` stays 200.
2. The listener keeps accepting for `server.drain_delay_secs` (default 0), for
   balancers that need a moment to notice. Platforms with a `preStop` hook can leave it
   at 0; behind a plain proxy 5 is a good value.
3. The listener closes (new connections are refused), every connection is told to
   finish its current request and close (idle keep-alive connections close at once),
   and what is still open after `server.shutdown_timeout_secs` (default 30) is cut off.
   The drain is logged with the number of connections cut off.
4. At the same moment the background-task token is cancelled and the tasks get the
   same deadline; stragglers are aborted.
5. Logs and traces are flushed and the process exits with code 0.

Kubernetes: set `terminationGracePeriodSeconds` to `shutdown_timeout_secs +
drain_delay_secs + 15`, and give the pod a `preStop` pause of at least the readiness
period times its failure threshold unless `drain_delay_secs` covers it.

## Endpoints owned by this layer

| Path | Behaviour |
|---|---|
| `GET /healthz` | Constant body `{"status":"ok"}`. Touches no dependency: not the database, Redis, blob store, Keycloak, LDAP or upstreams. |
| `GET /readyz` | 200 only when the node listens, is not draining, the schema is not older than the binary, and PostgreSQL answered `SELECT 1` within 1 second. The probe result is cached for 2 seconds so that polling cannot load the pool. Redis, Keycloak, LDAP, S3 and upstreams never change the status code (a probe may list them as `advisories`). The body names the checks and nothing else: no DSN, host or driver message. |
| `GET /metrics` | Prometheus text. 404 when disabled, open when `metrics_public`, otherwise requires `Authorization: Bearer <token>` (constant-time comparison). |

Why PostgreSQL decides readiness: without it a node cannot serve a single metadata
request, so leaving the load balancer is correct. The accepted price is a cascade,
every node turns NotReady together while the database is down. Liveness does not
depend on the database, so pods are not restarted and traffic returns by itself. The
same comment sits in `crates/rustryx-api/src/readiness.rs` and must be repeated next
to the `readinessProbe` in the Helm chart.

These three routes are outside the concurrency limit, the load shedder, the rate
limiter and authentication: a node saturated with uploads must still answer its
liveness probe.

## What every route gets

Layers, outermost first.

| Scope | Layer | Notes |
|---|---|---|
| all | request id | `X-Request-Id` is kept when 1 to 64 characters of `[A-Za-z0-9._-]`, otherwise replaced by a `UUIDv7`. Returned on every response and in every error body. |
| all | request span, metrics, log line | Span `http.request` with `request_id`, `method`, `route` (the matched template, never the path), `client_ip`, `user`, `repo`, `trace_id`, `span_id`, `status`. One `request completed` line per request (probes at debug). W3C `traceparent` is adopted as the parent. |
| all | security headers | `X-Content-Type-Options`, `Referrer-Policy`, `Strict-Transport-Security`, `X-Frame-Options` and a `sandbox` CSP, each only when the handler did not set its own. |
| all | panic recovery | A panicking handler becomes a 500 envelope; the panic message goes to the log, never to the client; the node keeps serving. |
| api, artifacts | load shed | One semaphore of `limits.max_in_flight_requests` shared by both groups. Beyond it: 503 `overloaded` with `Retry-After`, counted in `http_shed_total{reason="overload"}`. |
| api, artifacts | timeout | Response head within `limits.request_timeout_secs`, else 504 `request_timeout`. |
| api, artifacts | body limits | API: `limits.max_body_mb` (default 10). Artifacts: `limits.max_blob_bytes` (default 5 GiB). A declared `Content-Length` over the limit is refused before reading, a stream over the limit is cut off, a stream idle for `limits.body_idle_timeout_secs` is cut off. Answers 413 and 408. Artifact response bodies also get the idle timeout. |
| api | CORS | Exact origin list from `server.cors_allowed_origins`; none configured means no CORS headers. Credentials are not allowed. Applied outside authentication so a browser can read a 401. |
| api | gzip | Not applied to artifacts. |
| api | `Cache-Control: no-store` | Unless the handler chose its own caching. |
| api, artifacts | authentication | See below. |
| api, artifacts | rate limit | Token bucket per user, or per client address when anonymous (`limits.rate_limit`, default 50 rps burst 100). 429 `rate_limited` with `Retry-After`. Disable with `limits.rate_limit.enabled: false`. |

Not found and wrong-method answers are envelopes too (`route_not_found`,
`method_not_allowed` with `Allow`).

### Authentication

The middleware reads `Authorization: Basic|Bearer` (and nothing else: a token in the
query string is ignored), runs the authenticator chain from `AppState` and stores the
`Principal` in the request extensions.

- No header: anonymous, the authenticator is not called.
- A header that is present but unusable (unknown scheme, bad base64, empty or NUL
  password, duplicated header, token outside the RFC 6750 alphabet): 401, never
  anonymous.
- No authenticator wired but credentials presented: 503, never accepted.
- `AuthError` mapping: invalid credentials, disabled account, bad token: 401 with a
  `WWW-Authenticate` challenge (a disabled account answers exactly like a wrong
  password); lockout 429; provider unavailable, overload, login store down: 503 with
  `Retry-After`; group lookup failure: 403 (fail closed).
- The challenge defaults to `Basic realm="rustryx"`. The composition root can install
  a selector (`AppState::with_challenge_selector`) that answers `Bearer realm="pub"`
  on pub routes. A handler may also attach a challenge to its own 403.
- `AppState::authorize(principal, action, resource)` is the deny-by-default floor for
  handlers: no checker wired is 503, anonymous with the global switch off is 401, a
  deny is 401 for anonymous and 403 otherwise.

The client address comes from the TCP peer; `X-Forwarded-For` is believed only when
the peer is in `server.trusted_proxies`, read from the right.

### Errors

Every error is `{"error":{"code","message","request_id"}}`. `message` is written per
code and never repeats a backend error, so SQL, paths, upstream addresses and LDAP
details cannot reach a client. The detail is logged once, at the boundary, with the
source chain: server faults as `error`, expected client errors as `info`.

| Source | Status and code |
|---|---|
| `DomainError::NotFound{entity}` | 404 `<entity>_not_found` |
| `AlreadyExists`, `Conflict` | 409 `already_exists`, `conflict` |
| `Invalid` | 400 `invalid_input` |
| `Unauthenticated`, `Forbidden` | 401 `unauthenticated`, 403 `forbidden` |
| `QuotaExceeded` | 507 `quota_exceeded` |
| `Unavailable`, `StoreError::Unavailable`, `BlobError::Unavailable` | 503 `dependency_unavailable`, `store_unavailable`, `blob_store_unavailable` with `Retry-After` |
| `StoreError::NotFound`, `BlobError::NotFound` | 404 `not_found`, `blob_not_found` |
| `StoreError::Conflict` | 409 `conflict` |
| `BlobError::RangeNotSatisfiable` | 416 `range_not_satisfiable` |
| `Backend`, `Internal`, `WriterClosed` | 500 `internal_error` |
| upstream (constructors `upstream_error`, `upstream_timeout`) | 502, 504 |

Messages of `Invalid`, `Conflict` and `AlreadyExists` are passed to the client
(control characters stripped, 240 characters at most), so services must keep internals
out of them. Native protocol endpoints (npm, PyPI, OCI, pub) answer in their own
formats and only use the conversions to pick a status.

Extractors `ApiJson`, `ApiQuery` and `ApiPath` fail with the envelope (400, 413, 415)
instead of axum's plain text.

## Adding routes without editing this crate

```rust
struct MavenModule { /* its own state */ }

impl rustryx_api::RouteModule for MavenModule {
    fn name(&self) -> &'static str { "maven" }

    // Artifact routes: no gzip, large body limit, idle timeouts.
    fn mount_artifacts(&self, router: Router<AppState>) -> Router<AppState> {
        router.merge(self.routes().with_state(self.state.clone()))
    }
}
```

`Router::with_state(module_state)` turns a router over the module's own state into a
`Router<AppState>`, so no field has to be added to `AppState`. A module has two hooks:
`mount_api` (JSON metadata and management endpoints, for example `/service/rest/v1`)
and `mount_artifacts` (package protocols, `/repository/{name}/...`, `/v2/...`). Handlers
use `RequestPrincipal`, `AuthenticatedPrincipal`, `AppState::authorize`, `ApiError`,
`record_repo(name)` (puts `repo` on the request span and so on every log line) and
`ApiError::from_body_error` for body read failures. Add modules to
`wiring::Components::modules`.

## Logs, metrics, traces

- Logs: one JSON object per line on standard output, flat keys `ts`, `level`, `msg`,
  `target`, plus the fields of the enclosing request span. A non-blocking writer drops
  lines (counted in `log_lines_dropped_total`) instead of stalling the runtime when the
  consumer stalls. Every value passes a redactor: fields named like secrets are
  replaced, and `Bearer`/`Basic` credentials, `ryx_` tokens, JWTs, PEM private keys, URL
  user-info and `key=value` secrets are scrubbed from text. Values are capped at 2048
  characters. The filter is `observability.log_level` (`level` or `target=level`
  directives); spans of this workspace stay enabled whatever the level, so that a
  warning still carries its request id.
- Metrics: `http_requests_total{route,method,status}`,
  `http_request_duration_seconds{route,method}` (time to the response head, buckets 5 ms
  to 60 s), `http_requests_in_flight` (until the body finishes), `http_shed_total{reason}`,
  `auth_attempts_total{method,result}` and the helpers for upstream, pool and job metrics
  in `rustryx_observability::MetricsHandle`. The method label is bounded
  (unknown methods become `OTHER`), the route label is a template or `unmatched`.
  `process_*` and tokio runtime metrics are not emitted yet.
- Traces: off unless `observability.otlp_endpoint` is set. gRPC or HTTP, parent-based
  sampling at `otlp_sample_ratio`, W3C trace context in and (via
  `propagation::inject_current_context`) out. Only this workspace's spans at info or
  above are exported. Keeping every error trace is the collector's job (tail sampling).

## What the lead must wire

Everything below is concentrated in `crates/rustryx-server/src/wiring.rs`
(`assemble`, `migrator`) unless noted. Nothing here may block startup on an external
system other than PostgreSQL.

| Component | What to do |
|---|---|
| Database | Build the pool from `config.database` (`effective_direct_url()` for advisory locks, `LISTEN` and migrations), implement `rustryx_api::ReadinessProbe` (`SELECT 1` and schema version: `UpToDate`, `Ahead`, `Behind`, `Unknown`) and put it in `Components::readiness`. Spawn a sampler that calls `MetricsHandle::set_db_pool` and `record_db_pool_wait`. |
| Migrations | Implement `migrate::Migrator` over `rustryx-db` and return it from `wiring::migrator()`. `serve` must keep not migrating. |
| Blob stores | Build the registry from `config.blob` (seed of the default store), respect `limits.max_blob_bytes`, `blob.local_min_free_percent`, the S3 part size. |
| Authentication | Build the authenticator chain (local, token, internal JWT, OIDC bearer, LDAP) and the group resolver from `config.auth`; put the chain in `Components::authenticator`. OIDC discovery and JWKS fetches retry in the background. Own the semaphores `limits.effective_password_permits()` and `limits.effective_blocking_permits()` with the `limits.permit_wait()` deadline and answer `AuthError::Overloaded` when no permit is free. Emit `auth_attempts_total` in one place only: the middleware already does (method `unknown` on failures, because the chain does not say which authenticator refused); if the auth crate emits it too, remove one. |
| Authorization | Put the `RbacChecker` in `Components::rbac`. |
| Route modules | Mount `/service/rest/v1` (api), `/repository/{name}/...` and `/v2/...` (artifacts), one module per feature, in `Components::modules`. Install a challenge selector for pub (`Bearer realm="pub"` on both 401 and 403). |
| Background tasks | Run schedulers, cleanup, blob GC and the RBAC invalidation listener as `supervisor::BackgroundTask`s in `Components::background`; they get the shutdown token and are restarted with back-off if they panic or exit. Use `config.lock` for the backend (one per process) and renew every `lock.renew_interval()`. |
| Audit | Create the `AuditSink` and give it to the services; the HTTP layer already provides `request_id` and the client address (`RequestClientIp`) for audit events. |
| Upstream | Enforce `config.upstream` (scheme and host allowlist, CIDR allowlist for internal mirrors, SSRF filter after DNS resolution, redirects, timeouts, retries) in the upstream client; emit `upstream_*` metrics through `MetricsHandle`. |
| Config sections | `deny_unknown_fields` means a crate that needs a key the schema lacks (audit retention, docker upload limit, cleanup defaults, per-store settings) needs the key added in `rustryx-config` first (schema, key table, `RUSTRYX_BLESS_DOCS=1 cargo test -p rustryx-config docs_`, `config.example.yaml`). |

## Known gaps and decisions

- Basic credentials are accepted over plain HTTP. Rejecting them unless the request
  arrived over TLS (directly or via a trusted proxy header) needs a policy for the
  development compose setup and is left to the lead.
- Deny-by-default for routes is a convention (`RequestPrincipal`,
  `AuthenticatedPrincipal`, `AppState::authorize`), not enforced structurally: a handler
  that never authorizes would serve anyone. A guard that fails a response whose handler
  never authorized would enforce it; it was not built because public routes (status,
  `/v2/` ping, login) need an explicit opt-out design first.
- The concurrency limit (`limits.max_in_flight_requests`) covers a request until its
  response head is produced. A long download body is then bounded only by
  `limits.max_connections` and the body idle timeout; a separate cap on concurrent
  downloads (and per-user uploads) is not implemented.
- `/healthz` and `/readyz` share the API port. Under a connection flood up to
  `limits.max_connections` a probe has to queue like any client; the 10 second header
  timeout reaps slow clients. A separate internal probe port is a deployment option.
- TLS is terminated in front of the node. `Strict-Transport-Security` is sent regardless
  (browsers ignore it over plain HTTP).

## Running the tests

```
cargo test -p rustryx-config -p rustryx-observability -p rustryx-api -p rustryx-server
```

The server crate includes end-to-end tests that spawn the real binary, send it
SIGTERM and SIGINT and inspect exit codes, output and ports. They need a `kill`
executable in `PATH` (any Unix).
