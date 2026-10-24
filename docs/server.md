# Server

`rustryx` is the single binary of the project (`crates/rustryx-server`). This page
describes how it starts, serves and stops, what the HTTP layer
(`crates/rustryx-api`) guarantees to every route, how a module adds routes, and, at
the end, what the composition root wires and what it does not. Configuration is
described in [config.md](config.md).

## Commands

| Command | What it does | Exit codes |
|---|---|---|
| `rustryx serve` | Runs the server. Never changes the database schema. | 0 after a graceful stop, 78 invalid configuration, 1 any other failure (for example the port is taken) |
| `rustryx migrate` | Applies database migrations on `database.direct_url` (or `database.url`) under an advisory lock. The only place the schema changes. A node that is already serving turns ready by itself once the schema is there. | 0, 78, 1 |
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
5. `wiring::assemble` builds the components. It does not run migrations and does not
   wait for PostgreSQL, Keycloak, LDAP or S3: the pool connects lazily, Keycloak
   discovery runs in a background task and LDAP binds on the first login. What stops
   the process here is configuration that cannot work: a malformed key, an invalid
   provider setting, `lock.backend: redis` (no such backend exists in this build).
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
5. The final steps run in order, within a second budget of the same length: the session
   revocations that were waiting out their grace run now (`SessionManager::drain`), the
   format engine stops its cache fills, lets uploads finish committing and flushes the
   download counters (`FormatEngine::drain`), the blob stores finish their cleanup, and the
   database pool closes last because the steps before it still write through it.
6. Logs and traces are flushed and the process exits with code 0.

Kubernetes: the worst case of steps 3 to 5 is twice `shutdown_timeout_secs` (the drain
and the tasks share the first budget, the final steps get the second), so set
`terminationGracePeriodSeconds` to `2 * shutdown_timeout_secs + drain_delay_secs + 15`,
and give the pod a `preStop` pause of at least the readiness period times its failure
threshold unless `drain_delay_secs` covers it.

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

### Native authentication endpoints

Mounted by `NativeAuthModule` (`crates/rustryx-api/src/native/`), contract in
`docs/openapi/rustryx-api-v1.yaml`.

| Route | Requires | Notes |
|---|---|---|
| `GET /api/v1/auth/providers` | public | Which sign-in methods are on. |
| `POST /api/v1/auth/login` | public | Username and password, local or LDAP. Starts a cookie session. Subject to the HTTPS rule of Basic (below): 403 `https_required` before the Origin check and before the chain is called. Its CSRF defence is the `Origin` check (the origin of `server.base_url` or of `server.cors_allowed_origins`; the `Host` when no base URL is set). A password starting with `ryx_` is refused: a token must not open a UI session. |
| `GET /api/v1/auth/oidc/login`, `GET /api/v1/auth/oidc/callback` | public | Authorization code with PKCE. `return_to` is a plain absolute path, checked again on the way back. A failed callback redirects a browser to `/login?error=<code>` and answers anyone else with the envelope. |
| `POST /api/v1/auth/logout` | public | Idempotent. Revokes the token id, drops the user's cached principals on every node and clears the cookies. |
| `GET /api/v1/auth/session` | public | State of the session; `authenticated: false` for an anonymous caller. |
| `POST /api/v1/auth/session/refresh`, `GET /api/v1/auth/csrf` | interactive | Reissue and CSRF token. An API token is refused here. |
| `GET /api/v1/me` | public | The caller: roles, groups, permission summary, token policy. An anonymous caller gets what `nx-anonymous` may see. |

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

The middleware reads `Authorization: Basic|Bearer` and the session cookie (and nothing
else: a token in the query string is ignored), runs the authenticator chain from
`AppState` and stores the `Principal` in the request extensions.

- No credentials: anonymous, the authenticator is not called.
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
- `auth_attempts_total` is counted here, once per attempt, labelled with the method
  that refused (the chain reports it). The chain's own counter is switched off
  (`record_attempt_metrics: false`) so that no series is doubled.

**Passwords over plain HTTP.** TLS ends in front of the node, so a request is "HTTPS" only
when the peer is in `server.trusted_proxies` and the right-most `X-Forwarded-Proto` (or
`Forwarded: proto=`) says `https`. With `server.require_https` on (the default) HTTP
Basic credentials and the JSON password login (`POST /api/v1/auth/login`) from any other
request are refused with 403 `https_required`, before the chain sees them. Bearer tokens, the session cookie (which is `Secure`) and `/metrics`
are not subject to the guard. A development node on plain HTTP sets
`RUSTRYX_SERVER_REQUIRE_HTTPS=false`.

**Cookie session.** The web UI holds `__Host-rustryx_session` (the internal JWT,
`HttpOnly`, `Secure`, `SameSite=Lax`) and `__Host-rustryx_csrf` (readable by script,
`SameSite=Strict`); an SSO session also carries `__Host-rustryx_sso`. The session is
stateless, so any node can serve it.

| Limit | Value | Held by |
|---|---|---|
| Absolute | `auth.session.absolute_timeout_secs`, 8 hours at most | the token's `exp`, fixed at login and kept by every reissue |
| Idle | `auth.session.idle_timeout_secs`, 30 minutes at most | `iat + idle`, checked before the chain; a request that finds less than half of it left reissues the token in the same response |
| Group snapshot (SSO) | 5 minutes | the chain refuses an older snapshot (see the gap below) |
| Replaced token | 30 seconds after a reissue (`SessionSettings::revoke_grace`, at most 5 minutes) | a revocation task per replaced token id, see below |

**Replaced tokens end.** A reissue (transparent or `POST /auth/session/refresh`) revokes the token it
replaced, but only after a grace: a page sends many requests in parallel, and the ones already in flight
with the old cookie must not suddenly get 401. The revocation is a task on a tracker owned by
`SessionManager`, sleeping for the grace or until the manager is drained. It is scheduled only after the new
token was issued (a failed reissue leaves the old token alone), a failure to revoke is logged and never fails
the request, and parallel reissues from one old token share one task. `POST /auth/logout` revokes the current
token at once and, through an in-memory link from each token to the one it replaced, any predecessor still
waiting out its grace (a logout that lands on another node ends it when the issuing node's timer fires, so the
window is the grace at most). `SessionManager::drain` cancels the sleeps, so at shutdown the pending
revocations run immediately, and waits for them within a deadline; the server calls it first in the final
steps, before the database pool closes. A crash loses the revocations that were waiting: those tokens then
live to their own end, as they did before the grace existed.

A mutating request (`POST`, `PUT`, `PATCH`, `DELETE`) authenticated by the cookie must
carry the CSRF token in `X-CSRF-Token` (equal to the CSRF cookie and the token of this
session's id) and an `Origin` that is the service origin; otherwise 403 `csrf_rejected`.
The one exception is `POST /api/v1/auth/login`, which has no session yet and relies on
the `Origin` check alone. A stale cookie (idle, absolute, revoked, malformed) makes the
request anonymous and tells the handler why, so an endpoint that needs a session answers
`token_expired`; a failure of the dependency that verifies it is 503.

**Deny by default.** A route is declared with a `Requires` (`Public`, `Authenticated`,
`Interactive`, `Repo(action)`, `Project(permission)`, `App(area, action)`, `Admin`,
`Delegated(reason)`), the route layer enforces what needs no resource and hands the
handler an `Authorized<...>` proof as an extractor, and a handler that needs the
resource asks `AppState::authorize*` itself. The proof type cannot be built outside the
crate, and a route that is not `Public` whose handler answers 2xx or 3xx without any
`authorize*` having succeeded is answered 500 `authorization_not_enforced` instead. The
catalogue of declared routes is `RouterBuilder::build_with_catalog`, and a test walks it.
`AppState::authorize` itself: no checker wired is 503, anonymous with the global switch
off is 401, a deny is 401 for anonymous and 403 otherwise.

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

A feature is a `RouteModule`. It owns its state, and it declares each route with what the
caller must be. `crates/rustryx-api/src/native/` (`NativeAuthModule`) is the worked
example: state built from `RouteSet::app_state`, one `route(path, Requires, handler)` per
endpoint, a proof taken as an extractor, response types in one file (`dto.rs`) that a test
reads.

```rust
struct WidgetModule { widgets: Arc<dyn WidgetRepo> }

#[derive(Clone)]
struct WidgetApi { app: AppState, widgets: Arc<dyn WidgetRepo> }

impl RouteModule for WidgetModule {
    fn name(&self) -> &'static str { "widgets" }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let api = WidgetApi { app: routes.app_state().clone(), widgets: self.widgets.clone() };
        routes
            .route("/api/v1/widgets", Requires::Authenticated, get(list).with_state(api.clone()))
            // `Admin` is enforced before the handler; the proof proves it ran.
            .route("/api/v1/widgets/{name}", Requires::Admin, delete(remove).with_state(api))
    }
}

async fn remove(State(api): State<WidgetApi>, proof: Authorized<AdminGrant>, ApiPath(name): ApiPath<String>)
    -> Result<StatusCode, ApiError>
{
    api.widgets.delete(&name, proof.principal()).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

A module has two hooks: `mount_api` (JSON endpoints under `/api/v1`) and
`mount_artifacts` (package protocols). `RouteSet` has no way to add a route without a
`Requires`; the one opt-out, `RouteSet::delegated(reason, router)`, is for the format
engine, which authorizes per repository and coordinate and says so in the catalogue.
For `Repo(..)` and `Project(..)` the handler calls `AppState::authorize` or
`authorize_project` with the resource. Handlers use `RequestPrincipal`,
`AuthenticatedPrincipal`, `ApiError`, `record_repo(name)` (puts `repo` on the request
span and so on every log line) and `ApiError::from_body_error` for body read failures.
Add modules to `wiring::assemble` (the `modules` list).

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
  `http_request_duration_seconds{route,method,class,cache}` (time to the response head,
  buckets 5 ms to 60 s; `class` is `metadata`, `download`, `upload`, `api` or `other`, `cache`
  is `hit`, `miss`, `stale`, `bypass` or `none`, and the format engine decides both for
  `/repository`), `http_requests_in_flight` (until the body finishes),
  `http_shed_total{reason}`, `auth_attempts_total{method,result}`, `db_up`, the pool gauges,
  `process_*` (Linux, from `/proc`) and `tokio_*` (workers, busy ratio, queue depth, alive
  tasks), `background_job_*{task}`, and the helpers for upstream, pool and job metrics in
  `rustryx_observability::MetricsHandle`. The method label is bounded (unknown methods
  become `OTHER`), the route label is a template or `unmatched`. `tokio_blocking_queue_depth`
  needs `--cfg tokio_unstable` and is not exported; `redis_up` is not exported because no
  Redis backend exists in this build. The full catalogue is in `docs/slo.md`.
- Traces: off unless `observability.otlp_endpoint` is set. gRPC or HTTP, parent-based
  sampling at `otlp_sample_ratio`, W3C trace context in and (via
  `propagation::inject_current_context`) out. Only this workspace's spans at info or
  above are exported. Keeping every error trace is the collector's job (tail sampling).

## The composition root

`crates/rustryx-server/src/wiring.rs` is the one place that knows every crate; the pieces
are in `src/wiring/`. `assemble` returns `Components`; `serve::run_server` runs them.

| Component | Wired as | Notes |
|---|---|---|
| Database | lazy `PgPool` from `database.url`, `PgStore` over it | `PgReadiness`: `SELECT 1` plus `schema_status().is_serving()`, feeds `db_up`; a sampler publishes the pool gauges. `database.connect_timeout_secs` is not applied: the database crate has no knob for it (`acquire_timeout_secs` bounds the wait for a connection, connect included). |
| Migrations | `PgMigrator` (`rustryx migrate`) | On `database.direct_url`, advisory lock, forward only. `serve` never migrates. |
| Sealing | `AeadSealer` from the `database.sealing_*` ring | Production requires the key. In development without one a key for the process is generated and logged as a warning: sealed values do not survive a restart. |
| Locks and invalidation | `PgLockBackend` and `PgInvalidationBus` on the direct URL | One shared `LISTEN` connection fans out to the RBAC caches, the authentication caches and the blob registry (`SharedBus`). `lock.backend: redis` is refused at startup: no Redis backend exists. |
| Authentication | `AuthService` over `Passwords`, `InternalJwtService`, `LoginGuard`, `RoleMapper`, `JitProvisioner`, Keycloak and LDAP when enabled | Keys come from the rings (`auth.internal_jwt_key*`); development generates them when absent. `auth.jit_allowlist` feeds `jit_mode: allowlist`. Keycloak discovery is a supervised background task. |
| Sessions | `SessionManager` with `JwtSessionIssuer` | The issuer also drops the user's cached principals on every node when a session is revoked: the chain caches a verified principal for 5 minutes without looking at the revocation list. |
| Authorization | `Rbac` over the role and selector repositories | `auth.anonymous_enabled` is its global switch; an invalidation listener keeps its caches current. |
| Blob stores | `BlobStoreRegistry` over the definitions in the database, adapted to the format engine's trait | The `blob.*` section only SEEDS the default store, once, after the schema exists. The row rules afterwards. Secrets are named by environment variable in the row, never stored. |
| Format engine | `FormatEngine` with an EMPTY `FormatRegistry` | SSRF policy from `upstream.*`, routing rules through `RuleRoutingGate`, downloads counted by `DownloadBatcher` and flushed every `downloads.flush_interval_secs` and at shutdown. A request for an existing repository is answered by the engine with a 500 that says no handler is registered. Register a format in `wiring::formats::registry`. |
| Routes | `NativeAuthModule` under `/api/v1`, `ArtifactModule` for `/repository/{name}/...` and `/v2/...` | `/v2` answers 404 in the OCI error format (`UNSUPPORTED`) until an OCI format is registered. |
| First start | `bootstrap` task | Waits for a servable schema, then creates the bootstrap administrator (only when no administrator exists) and the default blob store, once, and parks. |
| Maintenance | `db_maintenance` task, one node at a time under a lock | Creates audit partitions ahead, purges expired revocations and stale login counters. It does NOT drop old audit partitions: that deletes evidence and needs an archive step first. |
| Samplers | pool and runtime samplers, metrics upkeep | `process_*` and `tokio_*`. |

Not wired: the other native API resources (repositories, users, roles, tokens, audit,
settings), any package format, an SSO session refresher (below), a Redis lock backend,
the task framework (`rustryx-tasks` is empty), GC and the schedulers. Each is a module or a
`BackgroundTask` added to `assemble`, and `Config sections` still applies: a crate that needs
a key the schema lacks adds it to `rustryx-config` first (schema, key table,
`RUSTRYX_BLESS_DOCS=1 cargo test -p rustryx-config docs_`, `config.example.yaml`).

## Known gaps and decisions

- **SSO sessions last five minutes.** The chain refuses a group snapshot older than five
  minutes, renewing it needs the Keycloak refresh token, and `OidcLoginFlow::complete` of
  `rustryx-auth` returns only the identity (the refresh token is discarded, and the crate
  has no refresh grant). Until `rustryx-auth` exposes both, a browser session started
  through Keycloak ends five minutes after login; local and LDAP sessions are not
  affected. The seam is ready: `SsoSessionRefresher` in `rustryx-api`, the sealed carrier
  cookie, the `SessionManager` parameter.
- The principal cache of the chain skips the revocation check for up to five minutes. Every
  path that revokes something must therefore also invalidate the user on the bus; the
  session logout does (`JwtSessionIssuer`), and so must token revocation and password
  change when their endpoints are written.
- The concurrency limit (`limits.max_in_flight_requests`) covers a request until its
  response head is produced. A long download body is then bounded only by
  `limits.max_connections` and the body idle timeout; a separate cap on concurrent
  downloads (and per-user uploads) is not implemented.
- `/healthz` and `/readyz` share the API port. Under a connection flood up to
  `limits.max_connections` a probe has to queue like any client; the 10 second header
  timeout reaps slow clients. A separate internal probe port is a deployment option.
- TLS is terminated in front of the node. `Strict-Transport-Security` is sent regardless
  (browsers ignore it over plain HTTP).
- Tokens issued to requests that reissue in parallel (one per in-flight request) are not revoked
  when the browser keeps only the last: each ends at its own idle end (30 minutes at most).

## Running the tests

```
cargo test -p rustryx-config -p rustryx-observability -p rustryx-api -p rustryx-server
```

The server crate includes end-to-end tests that spawn the real binary, send it
SIGTERM and SIGINT and inspect exit codes, output and ports. They need a `kill`
executable in `PATH` (any Unix).

`tests/postgres_serve.rs` runs the whole node against PostgreSQL 16 started with
testcontainers (Docker required): it starts the server on an empty database, checks
`/readyz` is 503, runs the migrations while the node is up, waits for ready, signs in as the
bootstrap administrator, calls `/api/v1/me`, exercises the CSRF, origin and password refusals,
reaches the format engine through `/repository`, scrapes `/metrics` and shuts down gracefully.
The container removes itself after ten minutes if the test is killed.
