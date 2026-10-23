# Configuration

Rustryx is configured by built-in defaults, an optional YAML file and environment
variables. The schema lives in the `rustryx-config` crate; a complete annotated
example is `config.example.yaml` in the repository root.

## Loading

| Step | Rule |
|---|---|
| File | `--config <path>` wins over `RUSTRYX_CONFIG`. With neither, only defaults and the environment are used. A path that cannot be read is an error. The file may be at most 1 MiB and its top level must be a mapping. |
| Precedence | environment variable, then file, then built-in default. |
| Environment names | A key `a.b.c` is overridden by `RUSTRYX_A_B_C`: the prefix `RUSTRYX_`, the dotted path with `.` replaced by `_`, upper-cased. Underscores inside a key name stay, so `auth.oidc.client_id` is `RUSTRYX_AUTH_OIDC_CLIENT_ID`. A double underscore is not needed and not used. A test proves that no two keys map to the same name. |
| Alias | `RUSTRYX_ENV` is accepted as an alias of `RUSTRYX_ENVIRONMENT`. Setting both to different values is an error. |
| Value syntax | booleans: `true`, `false`, `1`, `0`. Integers: decimal, non-negative. Lists: comma separated, whitespace around items ignored. Enums: the listed lowercase words, case-insensitive in the environment. |
| Empty and null | In the file, `null` or an empty value means "use the default". In the environment, an empty value unsets a key that has no default (`server.base_url`, `auth.oidc.issuer`, ...). Elsewhere an empty value is a real empty string and validation decides whether it is acceptable. |
| Secrets from files | Every secret key also accepts `RUSTRYX_..._FILE` holding the path of a file, for a Kubernetes Secret mounted as a volume. One trailing newline is removed, nothing else. A file larger than 1 MiB is refused. Setting both the plain variable and the `_FILE` variable is an error. |
| Unknown keys | Unknown keys in the file are errors, with a "did you mean" hint. Unknown `RUSTRYX_*` variables are warnings, because Kubernetes injects `RUSTRYX_SERVICE_*` and `RUSTRYX_PORT*` variables for a Service named `rustryx` (those two prefixes are skipped silently). |

All three layers are checked before the process opens a port. The whole list of
problems (unknown keys, wrong types, unparsable environment values, rule
violations) is printed in one pass and the process exits with code 78
(`EX_CONFIG`). Messages name the key, and the environment variable when that is
where the value came from; they never contain a configured value.

`rustryx print-config` prints the effective configuration as YAML. Every secret is
shown as `[REDACTED]` (or `null` when unset). `--no-validate` skips the rules but
not the structural checks, which is useful to inspect a configuration that does not
pass yet.

## Secrets

A secret is declared with the `ConfigSecret` type, whose `Debug` and `Serialize`
output is the placeholder. A new secret key therefore cannot leak through
`print-config` or a debug log by omission. Tests set a canary value in every
secret and search the YAML, `Debug` output and error messages for it, and a further
test checks that the keys the table calls secret are exactly the fields of that type.

## Rules worth knowing

These are refused by validation; each has a `must_not_*` test.

| Rule | Why |
|---|---|
| `environment: production` (the default) requires `database.url`, `auth.bootstrap_admin.password`, `auth.internal_jwt_key`, `auth.cookie_key` and a `metrics_token` (or `metrics_public: true`). | A node must not start half configured. |
| In production the bootstrap password is at least 12 characters and is not empty, not equal to the user name, not on the weak list (`admin`, `admin123`, `password`, `changeme`, `rustryx`, ...) and not such a word with digits or punctuation appended (`Admin123!`). In development a weak value only warns, an empty value is still an error. | No default credentials in production. |
| `auth.ldap.url` must be `ldaps://`, or `ldap://` together with `auth.ldap.starttls: true`. A plain `ldap://` fails even while LDAP is disabled. There is no plaintext flag. | Passwords must not cross the network in clear text. |
| `auth.oidc.default_role` must not be `nx-admin`. | Administrators come only from an exact match with `admin_groups`. |
| `auth.oidc.admin_groups` entries start with `/` (full Keycloak group paths). `auth.ldap.admin_groups` entries are DNs. | Matching is exact and keeps the leading slash. |
| `auth.oidc.scopes` includes `openid` and never `offline_access`. `groups_claim` is a top-level claim and not `realm_access` or `resource_access`. `clock_skew_secs` is at most 60. | Offline tokens are refused, client roles never assign product roles. |
| In production the OIDC issuer and `server.base_url` (when OIDC is on) use https. | JWKS come from HTTPS only. |
| `auth.session.idle_timeout_secs` is at most 1800 and `absolute_timeout_secs` at most 28800. `auth.ldap.groups_cache_ttl_secs` is at most 300. | Fixed upper bounds of the requirements. |
| `server.trusted_proxies` rejects `/0`. `server.cors_allowed_origins` rejects wildcards, paths and trailing slashes. | A `/0` lets any peer choose its client address; a wildcard exposes authenticated responses. |
| `upstream.allowed_schemes` includes `https`; `http` needs hosts in `allow_insecure_http_hosts`. `allow_private_cidrs` must not cover `/0`, loopback or `169.254.169.254`. | The SSRF allowlist exists for internal mirrors, never for instance metadata. |
| `gc.min_age_secs` is greater than `limits.request_timeout_secs` (when GC is enabled). | An upload may run for the whole request timeout before its blob is referenced; GC must not collect it. |
| `lock.backend: redis` requires `lock.redis_url`. | One backend per process life, no fallback. |

Warnings (printed at startup, never fatal): PostgreSQL without `sslmode=verify-full`
or `verify-ca` in production, `redis://` instead of `rediss://` in production, plain
http S3 endpoint or CORS origin or upstream in production, anonymous access enabled
in production, `lock.redis_url` set with the postgres backend, `/metrics` without a
token in development, unknown `RUSTRYX_*` environment variables.

## Resolved defaults

`limits.blocking_permits: 0` resolves to twice the CPUs available to the process
(`std::thread::available_parallelism`, which honours cgroup quotas).
`limits.password_permits: 0` resolves to the number of physical cores, capped by the
CPUs available to the process: inside a pod limited to two CPUs on a large node the
host's physical core count would otherwise let argon2id oversubscribe the quota.
`database.direct_url` defaults to `database.url`; it must bypass a transaction-mode
pooler because advisory locks and `LISTEN`/`NOTIFY` are session features.

## Format library

YAML is read with `serde_yaml_ng` (MIT), the maintained fork of the archived
`serde_yaml`. Layering needs an untyped tree: the file and environment layers are
merged as a `serde_yaml_ng::Value`, checked against the key table (unknown keys,
types), and only then deserialized into the typed schema, with `serde_path_to_error`
naming the failing key. A hand-written layer was chosen over `figment` or `config` because the behaviour
above (all errors in one pass, typo hints, `_FILE`, redaction by type, one key table
that also drives the docs) is simpler to guarantee over a tree this crate owns.

## Key reference

Generated from the key table and the default configuration by a test; edit
`crates/rustryx-config/src/keys.rs`, then run
`RUSTRYX_BLESS_DOCS=1 cargo test -p rustryx-config docs_` to refresh it.

<!-- BEGIN GENERATED KEY REFERENCE -->

| Key | Environment variable | Type | Default | Sensitivity | Description |
|---|---|---|---|---|---|
| `environment` | `RUSTRYX_ENVIRONMENT` | `production` / `development` | `"production"` | - | Deployment environment. `production` refuses default, empty and well-known weak credentials. |
| `server.listen_addr` | `RUSTRYX_SERVER_LISTEN_ADDR` | string | `"0.0.0.0:8081"` | - | Address of the single HTTP listener (API, formats, probes, metrics). |
| `server.base_url` | `RUSTRYX_SERVER_BASE_URL` | string | unset | - | Public base URL without path. Required when OIDC is enabled (the redirect URI is derived from it). |
| `server.shutdown_timeout_secs` | `RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS` | integer | `30` | - | Drain budget after SIGTERM, in seconds. |
| `server.drain_delay_secs` | `RUSTRYX_SERVER_DRAIN_DELAY_SECS` | integer | `0` | - | Seconds to keep serving after readiness flipped to 503, before the listener stops. |
| `server.trusted_proxies` | `RUSTRYX_SERVER_TRUSTED_PROXIES` | list | `[]` | - | CIDRs of peers allowed to set `X-Forwarded-For`. `/0` is refused. |
| `server.cors_allowed_origins` | `RUSTRYX_SERVER_CORS_ALLOWED_ORIGINS` | list | `[]` | - | Exact origins allowed by CORS. Empty sends no CORS headers. `*` is refused. |
| `database.url` | `RUSTRYX_DATABASE_URL` | string | unset | secret (also `RUSTRYX_DATABASE_URL_FILE`) | PostgreSQL URL (`postgres://...`). Required. |
| `database.direct_url` | `RUSTRYX_DATABASE_DIRECT_URL` | string | unset | secret (also `RUSTRYX_DATABASE_DIRECT_URL_FILE`) | Non-pooled URL for advisory locks, LISTEN/NOTIFY and migrations. Defaults to `database.url`. |
| `database.pool_size` | `RUSTRYX_DATABASE_POOL_SIZE` | integer | `20` | - | Connections per node (1 to 500). |
| `database.acquire_timeout_secs` | `RUSTRYX_DATABASE_ACQUIRE_TIMEOUT_SECS` | integer | `5` | - | Wait for a free connection before the request gets 503. |
| `database.connect_timeout_secs` | `RUSTRYX_DATABASE_CONNECT_TIMEOUT_SECS` | integer | `5` | - | TCP and TLS connect timeout. |
| `database.statement_timeout_secs` | `RUSTRYX_DATABASE_STATEMENT_TIMEOUT_SECS` | integer | `30` | - | `statement_timeout` for API connections. |
| `blob.store_name` | `RUSTRYX_BLOB_STORE_NAME` | string | `"default"` | - | Name of the default blob store. |
| `blob.kind` | `RUSTRYX_BLOB_KIND` | `local` / `s3` | `"local"` | - | Kind of the default blob store. |
| `blob.local_path` | `RUSTRYX_BLOB_LOCAL_PATH` | string | `"/var/lib/rustryx/blobs"` | - | Root directory of a local store. Absolute in production. |
| `blob.local_min_free_percent` | `RUSTRYX_BLOB_LOCAL_MIN_FREE_PERCENT` | integer | `10` | - | Free space percent below which a local store rejects writes (0 disables). |
| `blob.s3_endpoint` | `RUSTRYX_BLOB_S3_ENDPOINT` | string | unset | - | S3 endpoint URL. Required for `s3`. |
| `blob.s3_bucket` | `RUSTRYX_BLOB_S3_BUCKET` | string | unset | - | S3 bucket. Required for `s3`. |
| `blob.s3_region` | `RUSTRYX_BLOB_S3_REGION` | string | unset | - | S3 region. Required for `s3`. |
| `blob.s3_access_key` | `RUSTRYX_BLOB_S3_ACCESS_KEY` | string | unset | secret (also `RUSTRYX_BLOB_S3_ACCESS_KEY_FILE`) | S3 access key id. Required for `s3`. |
| `blob.s3_secret_key` | `RUSTRYX_BLOB_S3_SECRET_KEY` | string | unset | secret (also `RUSTRYX_BLOB_S3_SECRET_KEY_FILE`) | S3 secret access key. Required for `s3`. |
| `blob.s3_path_style` | `RUSTRYX_BLOB_S3_PATH_STYLE` | bool | `true` | - | Path-style addressing (MinIO, Ceph). |
| `blob.s3_part_size_mb` | `RUSTRYX_BLOB_S3_PART_SIZE_MB` | integer | `16` | - | Multipart part size in MiB (5 to 5120). |
| `auth.anonymous_enabled` | `RUSTRYX_AUTH_ANONYMOUS_ENABLED` | bool | `false` | - | Global switch for anonymous access. A repository must also allow it. |
| `auth.allow_local_role_supplement` | `RUSTRYX_AUTH_ALLOW_LOCAL_ROLE_SUPPLEMENT` | bool | `false` | - | Let manually assigned roles add to roles derived from groups. |
| `auth.jit_mode` | `RUSTRYX_AUTH_JIT_MODE` | `jit` / `allowlist` / `manual` | `"jit"` | - | Account creation policy for identity-provider users. |
| `auth.internal_jwt_key` | `RUSTRYX_AUTH_INTERNAL_JWT_KEY` | string | unset | secret (also `RUSTRYX_AUTH_INTERNAL_JWT_KEY_FILE`) | PEM Ed25519 private key signing internal JWTs. Required in production, same on every node. |
| `auth.cookie_key` | `RUSTRYX_AUTH_COOKIE_KEY` | string | unset | secret (also `RUSTRYX_AUTH_COOKIE_KEY_FILE`) | Base64 of 32 random bytes sealing the OIDC state cookie. Required in production, same on every node. |
| `auth.session.idle_timeout_secs` | `RUSTRYX_AUTH_SESSION_IDLE_TIMEOUT_SECS` | integer | `1800` | - | UI session idle timeout, at most 1800. |
| `auth.session.absolute_timeout_secs` | `RUSTRYX_AUTH_SESSION_ABSOLUTE_TIMEOUT_SECS` | integer | `28800` | - | UI session absolute lifetime, at most 28800. |
| `auth.bootstrap_admin.username` | `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_USERNAME` | string | `"admin"` | - | Login of the local break-glass administrator. |
| `auth.bootstrap_admin.password` | `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD` | string | unset | secret (also `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD_FILE`) | Initial password of that administrator. Required in production, at least 12 characters, not a known weak value. |
| `auth.oidc.enabled` | `RUSTRYX_AUTH_OIDC_ENABLED` | bool | `false` | - | Turn OIDC (Keycloak) login and bearer validation on. |
| `auth.oidc.issuer` | `RUSTRYX_AUTH_OIDC_ISSUER` | string | unset | - | Issuer URL, compared byte for byte with `iss`. https in production. |
| `auth.oidc.public_issuer_url` | `RUSTRYX_AUTH_OIDC_PUBLIC_ISSUER_URL` | string | unset | - | Issuer URL as seen by browsers when it differs from `issuer`. |
| `auth.oidc.client_id` | `RUSTRYX_AUTH_OIDC_CLIENT_ID` | string | unset | - | Client id of the confidential UI client, also the required `aud`. |
| `auth.oidc.client_secret` | `RUSTRYX_AUTH_OIDC_CLIENT_SECRET` | string | unset | secret (also `RUSTRYX_AUTH_OIDC_CLIENT_SECRET_FILE`) | Client secret of the confidential UI client. |
| `auth.oidc.admin_groups` | `RUSTRYX_AUTH_OIDC_ADMIN_GROUPS` | list | `[]` | - | Full group paths (leading `/`) granting `nx-admin` by exact match. |
| `auth.oidc.default_role` | `RUSTRYX_AUTH_OIDC_DEFAULT_ROLE` | string | unset | - | Role for users without a mapped group. Unset refuses them. `nx-admin` is refused. |
| `auth.oidc.groups_claim` | `RUSTRYX_AUTH_OIDC_GROUPS_CLAIM` | string | `"groups"` | - | Top-level claim holding group paths. |
| `auth.oidc.scopes` | `RUSTRYX_AUTH_OIDC_SCOPES` | list | `["openid", "profile", "email"]` | - | Scopes requested at login. Must include `openid`, must not include `offline_access`. |
| `auth.oidc.clock_skew_secs` | `RUSTRYX_AUTH_OIDC_CLOCK_SKEW_SECS` | integer | `60` | - | Clock skew for `exp`, `nbf`, `iat`, at most 60. |
| `auth.ldap.enabled` | `RUSTRYX_AUTH_LDAP_ENABLED` | bool | `false` | - | Turn LDAP login on. |
| `auth.ldap.url` | `RUSTRYX_AUTH_LDAP_URL` | string | unset | - | `ldaps://host[:port]`, or `ldap://host[:port]` with `starttls: true`. Plain `ldap://` fails validation. |
| `auth.ldap.starttls` | `RUSTRYX_AUTH_LDAP_STARTTLS` | bool | `false` | - | Upgrade an `ldap://` connection with StartTLS. |
| `auth.ldap.bind_dn` | `RUSTRYX_AUTH_LDAP_BIND_DN` | string | unset | - | Service account DN for the user search. |
| `auth.ldap.bind_password` | `RUSTRYX_AUTH_LDAP_BIND_PASSWORD` | string | unset | secret (also `RUSTRYX_AUTH_LDAP_BIND_PASSWORD_FILE`) | Service account password. |
| `auth.ldap.user_base_dn` | `RUSTRYX_AUTH_LDAP_USER_BASE_DN` | string | unset | - | Base DN of the user search. |
| `auth.ldap.search_filter` | `RUSTRYX_AUTH_LDAP_SEARCH_FILTER` | string | `"(uid={username})"` | - | User search filter containing `{username}`. |
| `auth.ldap.group_strategy` | `RUSTRYX_AUTH_LDAP_GROUP_STRATEGY` | `member_of` / `member_search` | `"member_of"` | - | How group membership is read. |
| `auth.ldap.group_attribute` | `RUSTRYX_AUTH_LDAP_GROUP_ATTRIBUTE` | string | `"memberOf"` | - | User attribute listing group DNs (`member_of`). |
| `auth.ldap.group_base_dn` | `RUSTRYX_AUTH_LDAP_GROUP_BASE_DN` | string | unset | - | Base DN of the group search (`member_search`). |
| `auth.ldap.group_filter` | `RUSTRYX_AUTH_LDAP_GROUP_FILTER` | string | `"(member={dn})"` | - | Group search filter containing `{dn}` (`member_search`). |
| `auth.ldap.admin_groups` | `RUSTRYX_AUTH_LDAP_ADMIN_GROUPS` | list | `[]` | - | Group DNs granting `nx-admin` by exact match. |
| `auth.ldap.ca_file` | `RUSTRYX_AUTH_LDAP_CA_FILE` | string | unset | - | PEM CA bundle for the server certificate. Must be readable when LDAP is enabled. |
| `auth.ldap.connect_timeout_secs` | `RUSTRYX_AUTH_LDAP_CONNECT_TIMEOUT_SECS` | integer | `5` | - | Connect timeout. |
| `auth.ldap.bind_timeout_secs` | `RUSTRYX_AUTH_LDAP_BIND_TIMEOUT_SECS` | integer | `5` | - | Bind timeout. |
| `auth.ldap.search_timeout_secs` | `RUSTRYX_AUTH_LDAP_SEARCH_TIMEOUT_SECS` | integer | `5` | - | Search timeout. |
| `auth.ldap.groups_cache_ttl_secs` | `RUSTRYX_AUTH_LDAP_GROUPS_CACHE_TTL_SECS` | integer | `300` | - | Lifetime of confirmed group membership, at most 300. |
| `auth.ldap.nested_group_depth` | `RUSTRYX_AUTH_LDAP_NESTED_GROUP_DEPTH` | integer | `5` | - | Maximum depth when expanding nested groups. |
| `auth.ldap.max_groups` | `RUSTRYX_AUTH_LDAP_MAX_GROUPS` | integer | `1000` | - | Maximum groups collected for one user. |
| `lock.backend` | `RUSTRYX_LOCK_BACKEND` | `postgres` / `redis` | `"postgres"` | - | Distributed lock backend for singleton jobs. Fixed for the life of the process. |
| `lock.redis_url` | `RUSTRYX_LOCK_REDIS_URL` | string | unset | secret (also `RUSTRYX_LOCK_REDIS_URL_FILE`) | Redis URL. Required for the `redis` backend. |
| `lock.ttl_secs` | `RUSTRYX_LOCK_TTL_SECS` | integer | `30` | - | Lock TTL, renewed every TTL/3 (5 to 300). |
| `limits.max_body_mb` | `RUSTRYX_LIMITS_MAX_BODY_MB` | integer | `10` | - | Largest JSON/metadata request body in MiB. |
| `limits.max_blob_bytes` | `RUSTRYX_LIMITS_MAX_BLOB_BYTES` | integer | `5368709120` | - | Largest artifact body in bytes; larger uploads get 413. |
| `limits.request_timeout_secs` | `RUSTRYX_LIMITS_REQUEST_TIMEOUT_SECS` | integer | `1800` | - | Ceiling for producing the response head of one request. |
| `limits.header_read_timeout_secs` | `RUSTRYX_LIMITS_HEADER_READ_TIMEOUT_SECS` | integer | `10` | - | Time a client may take to send request headers. |
| `limits.body_idle_timeout_secs` | `RUSTRYX_LIMITS_BODY_IDLE_TIMEOUT_SECS` | integer | `60` | - | Idle time allowed between body chunks. |
| `limits.max_connections` | `RUSTRYX_LIMITS_MAX_CONNECTIONS` | integer | `20000` | - | Open connections per node. |
| `limits.max_in_flight_requests` | `RUSTRYX_LIMITS_MAX_IN_FLIGHT_REQUESTS` | integer | `10000` | - | Concurrent requests per node; beyond this requests get 503. |
| `limits.blocking_permits` | `RUSTRYX_LIMITS_BLOCKING_PERMITS` | integer | `0` | - | Permits for blocking work. 0 means 2 x CPUs. |
| `limits.password_permits` | `RUSTRYX_LIMITS_PASSWORD_PERMITS` | integer | `0` | - | Permits for password hashing. 0 means physical cores. |
| `limits.permit_wait_ms` | `RUSTRYX_LIMITS_PERMIT_WAIT_MS` | integer | `1000` | - | Wait for a blocking permit before the request gets 503. |
| `limits.rate_limit.enabled` | `RUSTRYX_LIMITS_RATE_LIMIT_ENABLED` | bool | `true` | - | Turn the per-client rate limiter on. |
| `limits.rate_limit.requests_per_second` | `RUSTRYX_LIMITS_RATE_LIMIT_REQUESTS_PER_SECOND` | integer | `50` | - | Sustained requests per second per client. |
| `limits.rate_limit.burst` | `RUSTRYX_LIMITS_RATE_LIMIT_BURST` | integer | `100` | - | Burst size. |
| `upstream.allowed_schemes` | `RUSTRYX_UPSTREAM_ALLOWED_SCHEMES` | list | `["https"]` | - | Schemes a proxy `remote_url` may use. Must include `https`. |
| `upstream.allow_insecure_http_hosts` | `RUSTRYX_UPSTREAM_ALLOW_INSECURE_HTTP_HOSTS` | list | `[]` | - | Hosts reachable over plain `http`. Needed when `http` is an allowed scheme. |
| `upstream.allowed_hosts` | `RUSTRYX_UPSTREAM_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts (exact or `*.example.com`) may be upstreams. |
| `upstream.allow_private_cidrs` | `RUSTRYX_UPSTREAM_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed despite the SSRF filter. Loopback and metadata addresses are refused. |
| `upstream.max_redirects` | `RUSTRYX_UPSTREAM_MAX_REDIRECTS` | integer | `3` | - | Redirects followed per request, at most 3. |
| `upstream.connect_timeout_secs` | `RUSTRYX_UPSTREAM_CONNECT_TIMEOUT_SECS` | integer | `3` | - | Connect timeout. |
| `upstream.metadata_timeout_secs` | `RUSTRYX_UPSTREAM_METADATA_TIMEOUT_SECS` | integer | `30` | - | Timeout of metadata requests. |
| `upstream.idle_timeout_secs` | `RUSTRYX_UPSTREAM_IDLE_TIMEOUT_SECS` | integer | `60` | - | Idle timeout of a streaming body. |
| `upstream.max_attempts` | `RUSTRYX_UPSTREAM_MAX_ATTEMPTS` | integer | `3` | - | Attempts for GET and HEAD including the first, at most 3. |
| `gc.enabled` | `RUSTRYX_GC_ENABLED` | bool | `true` | - | Turn blob garbage collection on. |
| `gc.interval_secs` | `RUSTRYX_GC_INTERVAL_SECS` | integer | `3600` | - | Seconds between runs. |
| `gc.min_age_secs` | `RUSTRYX_GC_MIN_AGE_SECS` | integer | `86400` | - | A blob younger than this is never deleted. Must exceed `limits.request_timeout_secs`. |
| `gc.batch_size` | `RUSTRYX_GC_BATCH_SIZE` | integer | `1000` | - | Blobs examined per batch. |
| `observability.log_level` | `RUSTRYX_OBSERVABILITY_LOG_LEVEL` | string | `"info"` | - | Comma separated `level` or `target=level` directives. |
| `observability.log_format` | `RUSTRYX_OBSERVABILITY_LOG_FORMAT` | `json` / `text` | `"json"` | - | Log line format. |
| `observability.otlp_endpoint` | `RUSTRYX_OBSERVABILITY_OTLP_ENDPOINT` | string | unset | - | OTLP collector endpoint. Unset keeps trace export off. |
| `observability.otlp_protocol` | `RUSTRYX_OBSERVABILITY_OTLP_PROTOCOL` | `grpc` / `http` | `"grpc"` | - | OTLP transport. |
| `observability.otlp_sample_ratio` | `RUSTRYX_OBSERVABILITY_OTLP_SAMPLE_RATIO` | number | `0.1` | - | Head sampling ratio (0 to 1) for traces without a sampled parent. |
| `observability.service_name` | `RUSTRYX_OBSERVABILITY_SERVICE_NAME` | string | `"rustryx"` | - | `service.name` resource attribute. |
| `observability.metrics_enabled` | `RUSTRYX_OBSERVABILITY_METRICS_ENABLED` | bool | `true` | - | Serve `/metrics`. |
| `observability.metrics_public` | `RUSTRYX_OBSERVABILITY_METRICS_PUBLIC` | bool | `false` | - | Serve `/metrics` without a token. |
| `observability.metrics_token` | `RUSTRYX_OBSERVABILITY_METRICS_TOKEN` | string | unset | secret (also `RUSTRYX_OBSERVABILITY_METRICS_TOKEN_FILE`) | Bearer token for `/metrics`. Required unless `metrics_public`; at least 16 characters in production. |

<!-- END GENERATED KEY REFERENCE -->
