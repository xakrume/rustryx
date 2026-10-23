# Gaps in rustryx-core reported by wave 1 agents

Agents were not allowed to edit `rustryx-core`. This file collects what they
worked around, so the lead can change the contract once, after wave 1.
Status: `open` until the change lands in core.

| # | Reported by | Gap | Proposed change | Status |
|---|---|---|---|---|
| 1 | testkit | `StoreError` has no invalid-input variant: a malformed page token returns `NotFound` | add `StoreError::Invalid(String)` | open |
| 2 | testkit | `BlobRepo` has no insert method, unreferenced blobs are seeded outside the port | add `BlobRepo::insert` (publish already inserts blobs inside its transaction) | open |
| 3 | testkit | ports do not say what deleting a missing row returns, nor whether deleting a role or privilege cascades | document: missing row is `NotFound`, role and privilege delete cascades to link tables | open |
| 4 | testkit | `MemStore` implements many traits with the same method names, plain calls are ambiguous | consumers use `Arc<dyn Trait>` (documented, no core change) | closed |
| 5 | blob | no `BlobError::InvalidKey` (travels as `Backend` with a `KeyError` source) | add variant | open |
| 6 | blob | no disk-full variant (needed for 507) | add `BlobError::InsufficientStorage` | open |
| 7 | blob | no `Overloaded` variant, upload slot exhaustion uses `Unavailable` | add variant mapped to 503 + `Retry-After` | open |
| 8 | blob | no shutdown hook on the `BlobStore` port | add `async fn shutdown(&self)` with a default no-op | open |
| 9 | blob | `BlobMeta` docs promise digests but it holds only the size | fix the doc or add the checksums | open |
| 10 | blob | `BlobStoreRepo` has no `update` | add `update` | open |
| 11 | blob | the port cannot express "this backend cannot list stale uploads" (S3 via object_store) | make `list_stale_staging` return `Option` or document the empty result | open |
| 12 | rbac | `Coordinate` has no `extension` or `classifier`, so maven `coordinate.extension` selectors cannot compile | add both fields | open |
| 13 | rbac | `ResourceRef` carries neither `allow_anonymous` nor the global anonymous switch, so the HTTP layer must enforce both gates before calling rbac | document in `rustryx-api` and add a test there, or extend `ResourceRef` | open |
| 14 | rbac | `RbacChecker` has no admin-route method, rbac offers `Rbac::is_admin` instead | add `RbacChecker::is_admin` | open |
| 15 | rbac | `Principal::is_admin()` matches by role NAME, a role reference that only claims the name passes; rbac matches by role id against the store | remove the name-based method from core or document it as untrusted | open |
| 16 | oidc | `Principal` has no email field, JIT must use `OidcBearerAuthenticator::validate()` which returns `VerifiedBearer { identity, expires_at }` | add `email` to `Principal` or keep the validate() route | open |
| 17 | oidc | `rustryx-config` has no `authorized_parties`: CLI tokens carry `azp: rustryx-cli`, the strict `azp` rule refuses them | add `auth.oidc.authorized_parties` (default = `[client_id]`) | open |
| 18 | db | bad page tokens and out-of-range numbers come back as `Conflict` because `StoreError` lacks an invalid-input variant (same as gap 1) | add `StoreError::Invalid` and switch db and testkit to it | open |
| 19 | db | no read port for audit events (NDJSON export), no ports for job leases and system config, no entities or ports for cleanup policies and routing rules (ids are stored bare) | add `AuditReadRepo`, `JobLeaseRepo`, `SystemConfigRepo`, `CleanupPolicyRepo`, `RoutingRuleRepo` in wave 2 | open |
| 20 | auth-core | `AuthError` has no access-denied variant: "no mapped group and no default role" is reported as `GroupLookupFailed`, "not provisioned" as `AccountDisabled` | add `AuthError::AccessDenied` (maps to 403) | open |
| 21 | auth-core | no core port for revoked internal-JWT ids: `RevokedTokenStore` lives in `internal_jwt` with an in-memory impl, a PostgreSQL impl is missing | add a table and impl in `rustryx-db`, move the trait to core | open |
| 22 | auth-core | `AuthContext` has no request deadline (the gate uses `permit_wait`, 2 s default) | add `deadline: Option<Instant>` | open |
| 23 | auth-core | `ExternalGroups` has no `fetched_at`, the chain times the LDAP 5-minute window from receipt | add `fetched_at` | open |
| 24 | auth-core | `UserRepo::set_password_hash` has no compare-and-set, the rehash race is narrowed but not closed | add `set_password_hash_if(old_hash, new_hash)` | open |
| 25 | auth-core | `Principal` carries no email, display name or token expiry: JIT creates accounts without email, cache TTL reads `exp` from the JWT payload | extend `Principal` (see gap 16) | open |
| 26 | format-core | `AssetRepo::publish` has no create-only mode: `allow_once` and "a cache fill never replaces a published asset" are race-free only within one node; no way to refresh `updated_at`, so an expired cache entry means a full refetch | add `PublishMode::{Upsert, CreateOnly}` and `AssetRepo::touch_fresh` | open |
| 27 | format-core | no routing-rule port (`RoutingGate` seam is a stand-in) | add `RoutingRuleRepo` (see gap 19) | open |
| 28 | format-core | a committed but unpublished blob has no database row, a crash between commit and publish leaves an orphan the DB-driven GC cannot find | write the `blobs` row before commit as `pending`, or scan the store (see blob gap) | open |
| 29 | format-core | `BlobStore` has no resumable or compose upload, which OCI chunked uploads in HA need | design in wave 2 with the oci format (staging key + server-side compose, or multipart kept open across requests) | open |
| 30 | format-core | `ProxyConfig` has no per-repository CA bundle | add `ca_bundle_secret_ref` | open |
| 31 | runtime | `DomainError::{Invalid, Conflict, AlreadyExists}` strings reach clients verbatim, so services must keep internals out of them | document in core, add a lint-like test in `rustryx-api` over every service message | open |
| 32 | runtime | `AuthError` does not say which authenticator refused, so `auth_attempts_total` uses method `unknown` on failures | add a `method` to the failure variants or return it in a wrapper | open |
| 33 | runtime | core has no upstream error type | add `UpstreamError` (502 and 504 mapping) used by format-core and api | open |
| 34 | runtime | core `Secret` serializes its real value, config wraps secrets in its own redacting type | keep, but forbid `Secret` in response DTOs by a test in `rustryx-api` | open |

Open non-core issues from the blob agent: no disk-free 10% write rejection
(needs statvfs, `unsafe` is forbidden: solve through a metrics-based alert or a
safe crate), a lost S3 commit response can leave an object without a `blobs`
row (needs an orphan-object scan job against the bucket listing), memory is
`max_concurrent_uploads x part_size` (512 MiB by default) which conflicts with
the 500-upload target (lower the part size under load or the default cap), no
metrics emitted yet, minimal images need a CA bundle for the S3 TLS verifier.

Decision from the rbac agent (ADR-RBAC-1 in docs/rbac.md): the `cel` crate
(0.15.0) was rejected after a 24-expression corpus run: `=~` does not parse,
15 of 24 invalid expressions are accepted at save time, regexes are recompiled
per evaluation, there is no step limit and deep nesting aborts a debug build.
Rustryx uses its own `RestrictedEngine` behind `SelectorEngine`, which the spec
(02-rust-engineering section 2) names as the fallback. `=~` matches the whole
string and `matches()` any substring; neither was compared with the original
product.

## Integration to-do collected from reports

- Root manifest: `openidconnect = { version = "4", default-features = false }` removes the duplicate reqwest 0.12 (oidc agent built and tested that way). Hoist `hyper`, `hyper-util`, `http-body-util` dev-dependencies the oidc agent versioned locally.
- The auth chain must answer 401 for a Bearer credential that no authenticator claims (the OIDC authenticator returns `Ok(None)` for non-JWT, non-RS/ES and other-issuer tokens).
- OIDC wiring: `OidcProvider::new`, `spawn_background(CancellationToken)`, `bearer_authenticator()`, `group_resolver()`, `login_flow()`, `status()`. Config mapping `auth.oidc.*` to `OidcConfig` is in docs/oidc.md and the oidc report: issuer, public_issuer_url, client_id (audience and ui.client_id), client_secret, groups_claim, scopes, clock_skew_secs, auth.cookie_key, redirect URL from `server.base_url`.
- rbac wiring: `Rbac::new(role_repo, privilege_repo_or_selector_repo, &RbacConfig)`, store the listener from `spawn_invalidation_listener` and join it on shutdown.
- Commit `crates/rustryx-rbac/tests/*.proptest-regressions`.
- Metrics facade calls are missing in blob and oidc (status structs only).

## Helm and monitoring reconciliation (from the helm agent)

- Metric names are unprefixed as exported by `rustryx-observability` (`names.rs`), every alert selector carries `job="rustryx"`. If a `rustryx_` prefix is added later, sed `deploy/monitoring/*`, the chart `files/` copies and `docs/slo.md`.
- `http_request_duration_seconds` needs the extra labels `class` and `cache` (metadata vs artifact, hit vs miss), otherwise the metadata and TTFB latency alerts cannot fire.
- Series the alerts query but code does not export yet (enforced by `deploy/monitoring/check_assets.py`): `db_up`, `redis_up`, `redis_requests_total`, `blob_store_{ops_total,bytes_total,free_bytes,total_bytes}`, `background_job_running`, `distlock_acquire_total`, `ldap_requests_total`, `oidc_requests_total`, `oidc_jwks_age_seconds`, `tokio_{busy_ratio,global_queue_depth,blocking_queue_depth}`, `upstream_circuit_state`, `auth_breakglass_logins_total`.
- Label `job` on `background_job_*` collided with the scrape label (Prometheus renames it to `exported_job`): DONE, the code labels the job `task` and the rules, dashboard and runbook select `task`.
- Background job names in the staleness table (`blob_gc`, `cleanup`, `audit_rotation`) are guesses: align with `rustryx-tasks`. The last-success gauge must start at process start time.
- CI must publish `<version>-musl` and `<version>-glibc` tags (or change `image.variants`); prod values require `image.digest`.
- Config has no JWT key ring (only `auth.internal_jwt_key`): runbook documents single-key rotation with forced re-login. Decide whether to add a ring.
- `/metrics` shares the API port and is guarded only by its token (`RUSTRYX_OBSERVABILITY_METRICS_TOKEN`).
- Chart not exercised on a live cluster: probe paths via port-forward, hook ordering on first install, preStop on a kubelet, ESO interplay, Cilium FQDN policy, HPA adapter and alert wiring are unverified.

## Database integration notes (from the db agent)

- `/readyz` = `ping` (1 s) AND `schema_status().is_serving()`. Direct (non-pooler) URLs are required for `migrate`, `PgLockBackend` and `PgInvalidationBus`.
- A scheduler must run `run_audit_maintenance` and `purge_login_attempts` (owner credentials when the app role does not own the tables).
- The config layer must enforce `sslmode=verify-full` for database URLs.
- `PgStore::new` requires a `SecretSealer` (upstream passwords sealed with AES-256-GCM, key ring and reseal): needs a config key and a Secret in the chart.
- `--locked` fails until `Cargo.lock` is regenerated with the entries from other agents' manifests (rcgen, rustls, tokio-rustls): run `cargo update -w` once all agents are done.
- sqlx uses runtime queries (ADR in docs/db.md), PostgreSQL 15+ is required (`NULLS NOT DISTINCT`), usernames are case-insensitive and globally unique.
- Tests start `postgres:16-alpine` containers that self-terminate after 20 minutes (`RUSTRYX_DB_TEST_CONTAINER_TTL_SECS`).

## Auth integration notes (from the auth-core agent)

- Wiring example is in docs/auth-core.md (BlockingGate, Passwords, InternalJwtService with KeyRing, LoginGuard, RoleMapper, JitProvisioner, AuthService with `AuthServiceDeps`, `invalidation_task` on a JoinSet). `AuthService` never returns `Ok(None)`: anonymous credentials give `Principal::anonymous()`.
- The internal JWT carries a group snapshot (`grp`, `gat`) and the chain refuses a snapshot older than 5 minutes: the UI session layer must reissue the JWT whenever it refreshes the Keycloak token.
- `ryx_*` tokens are never principal-cached (immediate revocation). The login guard covers password Basic only. API tokens of OIDC owners get no OIDC-derived roles. `nx-admin` is never taken from `manual` rows for external users, and a mapping or `default_role` pointing at `nx-admin` or `nx-anonymous` is ignored.
- Not done: SEC-07 60-second cache for Basic passwords (each password request pays argon2id), the optional login-guard cache hook.
- Running `cargo fmt -p rustryx-auth` formats the ldap and oidc files too: run fmt once for the whole crate at integration.
- Added dependencies: workspace `ed25519-dalek` (pem), `num_cpus`; auth `arc-swap`, `uuid`, `metrics`.

## Format engine integration notes (from the format-core agent)

- `FormatEngine::builder(Ports, registry).limits().url_policy().shutdown_token().build()` then `serve(repo, RequestHead, Body) -> Response`. It runs classify, 405 for non-hosted writes, authorization (deny by default, fail closed), routing, handle. The API crate must parse the token and build `RequestHead`.
- `BlobStoreRegistry` in format-core is a TRAIT with `store_for(&Repository) -> Result<Arc<dyn BlobStore>, BlobError>`. `rustryx-blob` has its own struct with `store_for(BlobStoreId)`: an adapter is needed at wiring time (needs the repository's `blob_store_id` and the store definitions from `BlobStoreRepo`).
- Format authors: implement `FormatHandler`, `GroupIndexMerger` (pure sync, runs on the blocking pool under a 64 MiB cap) and `GroupNameClaimer`; register through `FormatRegistry::register`; use `cx.origin_repository()` for every URL written into documents. See docs/format-core.md.
- Known limits: followers of a cache miss wait for the full fill (bounded by the upstream idle timeout), a slow tee client holds an upstream permit until `client_stall_timeout`, the blob store definition is read on every write for quota checks (cache it), dual-stack internal hosts need both IPv4 and IPv6 ranges in `allow_cidrs`.
- Added workspace dependencies: `axum-core`, `dashmap`, `metrics`, `wiremock`.

## Runtime integration notes (from the runtime agent)

- Wiring checklist is in docs/server.md; everything goes into `crates/rustryx-server/src/wiring.rs`: DB pool, `ReadinessProbe` (`SELECT 1` plus schema version), pool metrics sampler, a real `Migrator`, blob registry, authenticator chain, group resolver, RBAC checker, audit sink, upstream client enforcing `config.upstream`, route modules for `/service/rest/v1`, `/repository`, `/v2` and a pub challenge selector, background tasks and the lock backend as `BackgroundTask`s. Missing config keys are added to the `rustryx-config` schema first (for example `authorized_parties`, the sealing key for upstream passwords).
- Exit code 78 on configuration errors. `RUSTRYX_ENV` is an alias of `RUSTRYX_ENVIRONMENT`. Logs use `ts` and `msg` keys with lowercase levels. Quota exceeded maps to 507.
- Open: Basic credentials are accepted over plain HTTP (TLS terminates at the ingress; add a startup warning or a `require_https` guard behind `trusted_proxies`), deny-by-default is a convention (`RequestPrincipal`, `AppState::authorize`) and not enforced structurally (wave 2: make handlers unable to touch content without an `Authorized` token type), the in-flight cap ends at the response head so there is no separate cap on concurrent downloads, `process_*` and tokio runtime metrics are not emitted, OTLP is only construction-tested, the header-read timeout applies to HTTP/1 only, probes share the API port, a non-verified PostgreSQL TLS mode is only a warning in production.
- Rate limiting per client was added beyond the brief.

## Delivery integration notes (from the ci agent)

- `crates/rustryx-server/Cargo.toml` needs `[[bin]] name = "rustryx"`: both Dockerfiles fail without it.
- The OIDC callback path `/api/v1/auth/oidc/callback` is assumed in the realm JSON: the api crate must serve it (or change the realm redirect URIs).
- Official MinIO images are gone from Docker Hub: compose pins `bitnamilegacy/minio`, the S3 integration test needs `RUSTRYX_MINIO_IMAGE` (blob agent fell back to `cgr.dev/chainguard/minio:latest`). Decide on one image for both.
- Release binary is built with `cargo auditable build --locked --release`, otherwise Trivy sees no Rust crates and the scan would be green while checking nothing.
- Image tags: `X.Y.Z-musl|glibc`, `sha-<7>-musl|glibc` on `ghcr.io/<owner>/rustryx` (matches chart `image.variant`). Repository variables: `RUSTRYX_SIGNING`, `COSIGN_KMS_KEY_REF`, `VAULT_ADDR`, `VAULT_JWT_ROLE`, `VAULT_JWT_AUDIENCE`, `IMAGE_SIZE_BUDGET_MB`, `RUSTRYX_MINIO_IMAGE`, `ENABLE_SEMGREP`. Key-based signing needs a committed `ci/cosign.pub`.
- Advisories: `RUSTSEC-2024-0449` (`unsafe-libyaml` via `serde_yaml_ng` in rustryx-config) is a warning, consider replacing the YAML crate; `RUSTSEC-2023-0071` (`rsa`) is an exception until 2027-01-22 enforced by `ci/check-exceptions.py`.
- Not run on a server: GitHub jobs, `.gitlab-ci.yml`, the `rustryx` compose profile, a build of the real workspace image. Only `slsa-github-generator@v2.1.0` is pinned by tag (the generator requires it).
- Not done: Makefile, devcontainer, `.pre-commit-config.yaml`, `security-exceptions.yml`, `SECURITY.md`.
- `rustryx-cli` tokens also get `aud=rustryx-ui` in the realm (otherwise the product could not accept them); `erin` and look-alike admin groups were added to both fixtures.

## LDAP integration notes (from the ldap agent)

- Wiring: copy `auth.ldap` field by field into `LdapConfig` (names are identical, aliases `start_tls`, `member_search`), `cfg.validate()?` (plaintext URL is an error, process must exit), `LdapService::new(&cfg, clock).await?`, then `LdapAuthenticator::new(svc.clone())` and `LdapGroupResolver::new(svc.clone())`. `svc.stats().snapshot()` feeds metrics.
- Stale-window conflict: `chain/roles.rs` reuses the last LDAP result for 5 minutes and stamps "now" on every `Ok`, while the LDAP resolver has its own cache (`groups_cache_ttl_secs`). Decision: the effective staleness must stay at most 5 minutes in total (README 8.5). Add `confirmed_at` to `ExternalGroups` (gap 23), make the chain use the resolver's `confirmed_at` instead of "now", keep the resolver strict (`groups_stale_on_error=false`) with a 60 s cache.
- `rustryx-config` documents `ca_file` as "in addition to system roots", the LDAP module trusts ONLY the file when set: align the docs in config. The config default `groups_cache_ttl_secs` is 300, the module default 60: set 60 in config.
- `Principal` has no user DN, so each group refresh searches for the user again (add `external_id` to Principal, see gaps 16 and 25).
- Pool or user-bind exhaustion ends as `ProviderUnavailable` after the deadline, never `Overloaded`.
- Not done: paged search above 1000 AD entries (such answers fail the lookup), referral following, several URLs for failover, a fuzz target for the filter builder.
- Both `ring` and `aws-lc-rs` are in the workspace, so `ClientConfig::builder()` would panic: providers are named explicitly everywhere (keep it so).
- Added workspace deps: `rustls`, `rustls-native-certs`, `rcgen`, `tokio-rustls`.

## Security review finding (automated, 24.10), open

- FIXED (address-keyed `auth_failure_guard` outside `authenticate`, 10 failed credentialed requests per address, 10 per minute refill, anonymous requests never charged, tests `must_not_allow_unlimited_credential_guessing_from_one_address`). Original finding: `crates/rustryx-api/src/router.rs`: the rate limiter sits inside `authenticate` (`with_rate_limit(...)` then `.layer(from_fn_with_state(state, authenticate))`), so failed logins can be sent without being counted per client IP. Fix: put a per-IP limiter (keyed on `ClientIp`) OUTSIDE `authenticate`, or charge the IP bucket on every failed attempt before returning 401; keep the per-user limiter for authenticated requests. Needs a `must_not_` test. The PostgreSQL-backed login guard in `rustryx-auth` already throttles password logins per (account, ip) and per account, so this covers token and bearer guessing and unauthenticated floods.

## RBAC adapted to core revision 2 (from the rbac agent)

- `Rbac::check` now owns both anonymous gates (repository flag and global switch) and refuses anonymous callers before any store read. Wire `RbacConfig.anonymous_enabled` from `auth.anonymous_enabled`; `Rbac::anonymous_switch()` gives a shared runtime handle.
- `is_admin`, `check_app`, `check_project`, `holds_whole_repository`, `visible_projects` are `RbacChecker` methods that fail closed (the trait must be in scope at call sites). Project roles and the robot scope rule are NOT implemented (stream W2-B); `check_project` passes only for `nx-admin` until the project access port is wired.
- A selector `coordinate.*` field that is `None` means unknown, not empty: a maven format must pass `Some("")` for "no classifier".
- New metrics, not yet in `names.rs`, `slo.md` or the dashboards: `rbac_decisions_total{kind,result}`, `rbac_cache_events_total{cache,event}`, `rbac_invalidations_total{event}`.
- Open core problems: `can_see_repository` has no repository flag in its signature (anonymous flag not applied there), `Coordinate` cannot tell "none" from "not derived", no project port reaches `Rbac`.
- `Cargo.lock` was edited by hand for the `metrics` edge of rbac: run `cargo update -w --offline` or a build and re-check `--locked` at integration.
