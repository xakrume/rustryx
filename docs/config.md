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
| `environment: production` (the default) requires `database.url`, `database.sealing_key`, `auth.bootstrap_admin.password`, `auth.internal_jwt_key`, `auth.cookie_key` and a `metrics_token` (or `metrics_public: true`). | A node must not start half configured. |
| In production `database.url` and `database.direct_url` use `sslmode=verify-full`. `verify-ca`, `require`, `prefer`, `disable` and a missing `sslmode` are refused. A Unix socket (`host=/var/run/postgresql`) is exempt. | `verify-ca` accepts any certificate the CA ever signed, whatever host it names. The message names the rule and never the URL (it carries the password). |
| `database.sealing_key` is 64 hexadecimal characters, not all zeros. `database.sealing_key_id` is 1 to 32 characters of letters, digits, `_` and `-` (no dot: the sealed envelope is split on dots). `database.sealing_previous_keys` is `id:hex` entries, at most 8, none with the current id. | A sealed value must be found again after a rotation, and a template value must not seal real data. |
| `auth.internal_jwt_key_id` is 1 to 64 characters of letters, digits, `.`, `_` and `-`. `auth.internal_jwt_previous_keys` is `kid: <id>` lines each followed by one PEM block, at most 8, none with the current id. | One `kid` names one key. |
| In production the bootstrap password is at least 12 characters and is not empty, not equal to the user name, not on the weak list (`admin`, `admin123`, `password`, `changeme`, `rustryx`, ...) and not such a word with digits or punctuation appended (`Admin123!`). In development a weak value only warns, an empty value is still an error. | No default credentials in production. |
| `auth.ldap.url` must be `ldaps://`, or `ldap://` together with `auth.ldap.starttls: true`. A plain `ldap://` fails even while LDAP is disabled. There is no plaintext flag. | Passwords must not cross the network in clear text. |
| `auth.oidc.default_role` must not be `nx-admin`. | Administrators come only from an exact match with `admin_groups`. |
| `auth.oidc.admin_groups` entries start with `/` (full Keycloak group paths). `auth.ldap.admin_groups` entries are DNs. | Matching is exact and keeps the leading slash. |
| `auth.oidc.scopes` includes `openid` and never `offline_access`. `groups_claim` is a top-level claim and not `realm_access` or `resource_access`. `clock_skew_secs` is at most 60. | Offline tokens are refused, client roles never assign product roles. |
| `auth.oidc.authorized_parties` entries are client ids without whitespace or commas. The effective list is `client_id` plus the listed parties. | The strict `azp` rule stays; a second client (the CLI) is an explicit allowlist entry, never implicit, and a list that names only the CLI cannot lock the UI out. |
| The OIDC issuer and `public_issuer_url` use https in every environment. `server.base_url` (when OIDC is on) uses https in production; in development plain http is accepted only on `localhost`, `127.0.0.1` or `[::1]`, or when no client secret is set (no browser login, so no redirect URI). | JWKS come from HTTPS only, and `rustryx-auth` refuses an http issuer or a non-loopback http redirect URI at startup: the configuration must not validate what the node then rejects. |
| `auth.session.idle_timeout_secs` is at most 1800 and `absolute_timeout_secs` at most 28800. `auth.ldap.groups_cache_ttl_secs` is at most 300 (60 by default). | Fixed upper bounds of the requirements. The authentication chain accepts a group confirmation for five minutes in total, so the directory cache has to stay well below that. |
| `server.trusted_proxies` rejects `/0`. `server.cors_allowed_origins` rejects wildcards, paths and trailing slashes. | A `/0` lets any peer choose its client address; a wildcard exposes authenticated responses. |
| `upstream.allowed_schemes` includes `https`; `http` needs hosts in `allow_insecure_http_hosts`. `allow_private_cidrs` must not cover `/0`, loopback or `169.254.169.254`. | The SSRF allowlist exists for internal mirrors, never for instance metadata. |
| `gc.min_age_secs` is greater than `limits.request_timeout_secs` (when GC is enabled). | An upload may run for the whole request timeout before its blob is referenced; GC must not collect it. |
| `lock.backend: redis` requires `lock.redis_url`. | One backend per process life, no fallback. |
| `tasks.log_retention_days` is at most `tasks.run_retention_days`. `downloads.retention_days` is at least 35. | A log is deleted with its run. A monthly view needs a month of daily counters plus slack. |
| Every `egress.<purpose>` policy follows the `upstream` rules: schemes include `https`, `http` needs hosts in `allow_insecure_http_hosts`, host entries are plain, `allowed_ports` is not empty and has no port 0, `allow_private_cidrs` never covers `/0`, loopback or `169.254.169.254`. | One policy per purpose, so a webhook is never allowed what an internal mirror is. The keys are file or environment configuration and not API editable, because they are the security boundary. |

Warnings (printed at startup, never fatal): `redis://` instead of `rediss://` in
production, plain http S3 endpoint or CORS origin or upstream or egress target in
production, anonymous access enabled in production, `lock.redis_url` set with the
postgres backend, `/metrics` without a token in development, no `database.sealing_key`
in development (credentials of proxy repositories cannot be stored then),
`server.require_https: false` in production, `server.require_https: true` with an empty
`server.trusted_proxies` (no request can count as HTTPS, so Basic credentials are
always refused), unknown `RUSTRYX_*` environment variables.

## Basic credentials over plain HTTP

TLS ends in front of the node. `server.require_https` (default `true`) refuses HTTP Basic
credentials on a request that did not arrive over HTTPS: the answer is 403 `https_required`
and the authenticator is not called, so a refused request cannot be used to guess a password.
A request counts as HTTPS when its direct peer is in `server.trusted_proxies` and the
right-most `X-Forwarded-Proto` value (or the right-most `Forwarded` element's `proto=`) is
`https`; the node believes these headers from nobody else, exactly like `X-Forwarded-For`.
The right-most value is the one the proxy directly in front of the node wrote. Bearer
tokens, the session cookie (which is `Secure`) and `/metrics` are not subject to this guard.
For local work on plain HTTP set `RUSTRYX_SERVER_REQUIRE_HTTPS=false`.

## Key rings

Two rings let a key rotate without an outage. The JWT ring is `auth.internal_jwt_key` (current,
with `auth.internal_jwt_key_id`) plus `auth.internal_jwt_previous_keys`; the sealing ring is
`database.sealing_key` (with `database.sealing_key_id`) plus `database.sealing_previous_keys`.
Procedure: deploy the new key as current under a new id with the old key listed as previous,
wait until nothing signed or sealed with the old key is left (one session lifetime of 8 hours
for JWTs, the reseal job for sealed values), then remove the old entry. The entry formats are in
the key reference below and in `config.example.yaml`.

## Wave 2 foundation sections

`tasks.*` (task framework), `outbox.*` (event outbox) and `downloads.*` (download counters) are
bootstrap values; operator tunable policy lives in database settings (ADR-W2-24 of
`wave2-design.md`). `egress.registries`, `egress.webhooks`, `egress.scanners`, `egress.probe`
(each with five keys), `egress.smtp` and `egress.proxy` are the outbound security boundary of
purposes other than proxy repositories. The `scan.*`, `notify.*`, `replication.*` and
`webhooks.*` keys belong to their streams and are added by them.

## Resolved defaults

`limits.blocking_permits: 0` resolves to twice the CPUs available to the process
(`std::thread::available_parallelism`, which honours cgroup quotas).
`limits.password_permits: 0` resolves to the number of physical cores, capped by the
CPUs available to the process: inside a pod limited to two CPUs on a large node the
host's physical core count would otherwise let argon2id oversubscribe the quota.
`database.direct_url` defaults to `database.url`; it must bypass a transaction-mode
pooler because advisory locks and `LISTEN`/`NOTIFY` are session features. The server uses it
for migrations, the lock backend and the invalidation bus, and `database.url` for requests.
`auth.ldap.ca_file`, when set, REPLACES the system roots for the LDAP connection: the LDAP
client trusts only the certificates in that file.

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
| `server.require_https` | `RUSTRYX_SERVER_REQUIRE_HTTPS` | bool | `true` | - | Refuse HTTP Basic credentials unless the request arrived over HTTPS: the peer must be in `trusted_proxies` and send `X-Forwarded-Proto: https`. |
| `database.url` | `RUSTRYX_DATABASE_URL` | string | unset | secret (also `RUSTRYX_DATABASE_URL_FILE`) | PostgreSQL URL (`postgres://...`). Required. |
| `database.direct_url` | `RUSTRYX_DATABASE_DIRECT_URL` | string | unset | secret (also `RUSTRYX_DATABASE_DIRECT_URL_FILE`) | Non-pooled URL for advisory locks, LISTEN/NOTIFY and migrations. Defaults to `database.url`. |
| `database.pool_size` | `RUSTRYX_DATABASE_POOL_SIZE` | integer | `20` | - | Connections per node (1 to 500). |
| `database.acquire_timeout_secs` | `RUSTRYX_DATABASE_ACQUIRE_TIMEOUT_SECS` | integer | `5` | - | Wait for a free connection before the request gets 503. |
| `database.connect_timeout_secs` | `RUSTRYX_DATABASE_CONNECT_TIMEOUT_SECS` | integer | `5` | - | TCP and TLS connect timeout. |
| `database.statement_timeout_secs` | `RUSTRYX_DATABASE_STATEMENT_TIMEOUT_SECS` | integer | `30` | - | `statement_timeout` for API connections. |
| `database.sealing_key` | `RUSTRYX_DATABASE_SEALING_KEY` | string | unset | secret (also `RUSTRYX_DATABASE_SEALING_KEY_FILE`) | AES-256 key as 64 hexadecimal characters sealing secrets stored in the database (upstream passwords). Required in production, same on every node. |
| `database.sealing_key_id` | `RUSTRYX_DATABASE_SEALING_KEY_ID` | string | `"k1"` | - | Id written into every sealed value (1 to 32 characters of letters, digits, `_` and `-`). |
| `database.sealing_previous_keys` | `RUSTRYX_DATABASE_SEALING_PREVIOUS_KEYS` | string | unset | secret (also `RUSTRYX_DATABASE_SEALING_PREVIOUS_KEYS_FILE`) | Retired sealing keys that still open old values, as `id:hex` entries separated by commas, spaces or new lines (at most 8). |
| `blob.store_name` | `RUSTRYX_BLOB_STORE_NAME` | string | `"default"` | - | Name of the default blob store. |
| `blob.kind` | `RUSTRYX_BLOB_KIND` | `local` / `s3` | `"local"` | - | Kind of the default blob store. |
| `blob.local_path` | `RUSTRYX_BLOB_LOCAL_PATH` | string | `"/var/lib/rustryx/blobs"` | - | Root directory of a local store. Absolute in production. |
| `blob.local_min_free_percent` | `RUSTRYX_BLOB_LOCAL_MIN_FREE_PERCENT` | integer | `10` | - | Free space percent below which a local store rejects writes (0 disables). |
| `blob.s3_endpoint` | `RUSTRYX_BLOB_S3_ENDPOINT` | string | unset | - | S3 endpoint URL. Required for `s3`. |
| `blob.s3_bucket` | `RUSTRYX_BLOB_S3_BUCKET` | string | unset | - | S3 bucket. Required for `s3`. |
| `blob.s3_region` | `RUSTRYX_BLOB_S3_REGION` | string | unset | - | S3 region. Required for `s3`. |
| `blob.s3_access_key` | `RUSTRYX_BLOB_S3_ACCESS_KEY` | string | unset | secret (also `RUSTRYX_BLOB_S3_ACCESS_KEY_FILE`) | S3 access key id. Required for `s3`. |
| `blob.s3_secret_key` | `RUSTRYX_BLOB_S3_SECRET_KEY` | string | unset | secret (also `RUSTRYX_BLOB_S3_SECRET_KEY_FILE`) | S3 secret access key. Required for `s3`. |
| `blob.s3_path_style` | `RUSTRYX_BLOB_S3_PATH_STYLE` | bool | `true` | - | Path-style addressing (SeaweedFS, Ceph). |
| `blob.s3_part_size_mb` | `RUSTRYX_BLOB_S3_PART_SIZE_MB` | integer | `16` | - | Multipart part size in MiB (5 to 5120). |
| `auth.anonymous_enabled` | `RUSTRYX_AUTH_ANONYMOUS_ENABLED` | bool | `false` | - | Global switch for anonymous access. A repository must also allow it. |
| `auth.allow_local_role_supplement` | `RUSTRYX_AUTH_ALLOW_LOCAL_ROLE_SUPPLEMENT` | bool | `false` | - | Let manually assigned roles add to roles derived from groups. |
| `auth.jit_mode` | `RUSTRYX_AUTH_JIT_MODE` | `jit` / `allowlist` / `manual` | `"jit"` | - | Account creation policy for identity-provider users. |
| `auth.jit_allowlist` | `RUSTRYX_AUTH_JIT_ALLOWLIST` | list | `[]` | - | Usernames that may get an account when `jit_mode` is `allowlist` (exact, case sensitive). |
| `auth.internal_jwt_key` | `RUSTRYX_AUTH_INTERNAL_JWT_KEY` | string | unset | secret (also `RUSTRYX_AUTH_INTERNAL_JWT_KEY_FILE`) | PEM Ed25519 private key signing internal JWTs. Required in production, same on every node. |
| `auth.internal_jwt_key_id` | `RUSTRYX_AUTH_INTERNAL_JWT_KEY_ID` | string | `"k1"` | - | `kid` of the current internal JWT signing key (1 to 64 characters of letters, digits, `.`, `_` and `-`). |
| `auth.internal_jwt_previous_keys` | `RUSTRYX_AUTH_INTERNAL_JWT_PREVIOUS_KEYS` | string | unset | secret (also `RUSTRYX_AUTH_INTERNAL_JWT_PREVIOUS_KEYS_FILE`) | Retired JWT keys that still verify tokens: `kid: <id>` lines each followed by one PEM block, public keys preferred (at most 8). |
| `auth.cookie_key` | `RUSTRYX_AUTH_COOKIE_KEY` | string | unset | secret (also `RUSTRYX_AUTH_COOKIE_KEY_FILE`) | Base64 of 32 random bytes sealing the OIDC state cookie. Required in production, same on every node. |
| `auth.session.idle_timeout_secs` | `RUSTRYX_AUTH_SESSION_IDLE_TIMEOUT_SECS` | integer | `1800` | - | UI session idle timeout, at most 1800. |
| `auth.session.absolute_timeout_secs` | `RUSTRYX_AUTH_SESSION_ABSOLUTE_TIMEOUT_SECS` | integer | `28800` | - | UI session absolute lifetime, at most 28800. |
| `auth.bootstrap_admin.username` | `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_USERNAME` | string | `"admin"` | - | Login of the local break-glass administrator. |
| `auth.bootstrap_admin.password` | `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD` | string | unset | secret (also `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD_FILE`) | Initial password of that administrator. Required in production, at least 12 characters, not a known weak value. |
| `auth.oidc.enabled` | `RUSTRYX_AUTH_OIDC_ENABLED` | bool | `false` | - | Turn OIDC (Keycloak) login and bearer validation on. |
| `auth.oidc.issuer` | `RUSTRYX_AUTH_OIDC_ISSUER` | string | unset | - | Issuer URL, compared byte for byte with `iss`. Always https. |
| `auth.oidc.public_issuer_url` | `RUSTRYX_AUTH_OIDC_PUBLIC_ISSUER_URL` | string | unset | - | Issuer URL as seen by browsers when it differs from `issuer`. |
| `auth.oidc.client_id` | `RUSTRYX_AUTH_OIDC_CLIENT_ID` | string | unset | - | Client id of the confidential UI client, also the required `aud`. |
| `auth.oidc.client_secret` | `RUSTRYX_AUTH_OIDC_CLIENT_SECRET` | string | unset | secret (also `RUSTRYX_AUTH_OIDC_CLIENT_SECRET_FILE`) | Client secret of the confidential UI client. |
| `auth.oidc.admin_groups` | `RUSTRYX_AUTH_OIDC_ADMIN_GROUPS` | list | `[]` | - | Full group paths (leading `/`) granting `nx-admin` by exact match. |
| `auth.oidc.default_role` | `RUSTRYX_AUTH_OIDC_DEFAULT_ROLE` | string | unset | - | Role for users without a mapped group. Unset refuses them. `nx-admin` is refused. |
| `auth.oidc.groups_claim` | `RUSTRYX_AUTH_OIDC_GROUPS_CLAIM` | string | `"groups"` | - | Top-level claim holding group paths. |
| `auth.oidc.scopes` | `RUSTRYX_AUTH_OIDC_SCOPES` | list | `["openid", "profile", "email"]` | - | Scopes requested at login. Must include `openid`, must not include `offline_access`. |
| `auth.oidc.clock_skew_secs` | `RUSTRYX_AUTH_OIDC_CLOCK_SKEW_SECS` | integer | `60` | - | Clock skew for `exp`, `nbf`, `iat`, at most 60. |
| `auth.oidc.authorized_parties` | `RUSTRYX_AUTH_OIDC_AUTHORIZED_PARTIES` | list | `[]` | - | Accepted `azp` values besides `client_id` (for example `rustryx-cli`). Empty means only `client_id`. |
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
| `auth.ldap.groups_cache_ttl_secs` | `RUSTRYX_AUTH_LDAP_GROUPS_CACHE_TTL_SECS` | integer | `60` | - | Lifetime of confirmed group membership, at most 300. |
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
| `tasks.max_concurrent_runs` | `RUSTRYX_TASKS_MAX_CONCURRENT_RUNS` | integer | `4` | - | Task runs one node executes at once (1 to 64). |
| `tasks.scheduler_tick_secs` | `RUSTRYX_TASKS_SCHEDULER_TICK_SECS` | integer | `15` | - | Seconds between looks at the schedule for due tasks (1 to 300). |
| `tasks.run_retention_days` | `RUSTRYX_TASKS_RUN_RETENTION_DAYS` | integer | `90` | - | Days a finished task run stays in the history (1 to 3650). |
| `tasks.log_retention_days` | `RUSTRYX_TASKS_LOG_RETENTION_DAYS` | integer | `30` | - | Days the log of a run is kept, at most `tasks.run_retention_days`. |
| `outbox.retention_days` | `RUSTRYX_OUTBOX_RETENTION_DAYS` | integer | `30` | - | Days events stay in the outbox before their partition is dropped (1 to 3650). |
| `outbox.poll_interval_ms` | `RUSTRYX_OUTBOX_POLL_INTERVAL_MS` | integer | `1000` | - | Milliseconds between polls of the outbox by a consumer (50 to 60000). |
| `downloads.flush_interval_secs` | `RUSTRYX_DOWNLOADS_FLUSH_INTERVAL_SECS` | integer | `10` | - | Seconds between flushes of the download counters to PostgreSQL (1 to 3600). |
| `downloads.retention_days` | `RUSTRYX_DOWNLOADS_RETENTION_DAYS` | integer | `90` | - | Days of daily download counters kept (35 to 3650). |
| `egress.registries.allowed_schemes` | `RUSTRYX_EGRESS_REGISTRIES_ALLOWED_SCHEMES` | list | `["https"]` | - | Schemes for: registry endpoints and replication. Must include `https`. |
| `egress.registries.allow_insecure_http_hosts` | `RUSTRYX_EGRESS_REGISTRIES_ALLOW_INSECURE_HTTP_HOSTS` | list | `[]` | - | Hosts reachable over plain `http` for: registry endpoints and replication. |
| `egress.registries.allowed_hosts` | `RUSTRYX_EGRESS_REGISTRIES_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts (exact or `*.example.com`) may be targets of: registry endpoints and replication. |
| `egress.registries.allowed_ports` | `RUSTRYX_EGRESS_REGISTRIES_ALLOWED_PORTS` | list of integers | `[443]` | - | Destination ports for: registry endpoints and replication. |
| `egress.registries.allow_private_cidrs` | `RUSTRYX_EGRESS_REGISTRIES_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed despite the SSRF filter for: registry endpoints and replication. Loopback and metadata addresses are refused. |
| `egress.webhooks.allowed_schemes` | `RUSTRYX_EGRESS_WEBHOOKS_ALLOWED_SCHEMES` | list | `["https"]` | - | Schemes for: webhook deliveries. Must include `https`. |
| `egress.webhooks.allow_insecure_http_hosts` | `RUSTRYX_EGRESS_WEBHOOKS_ALLOW_INSECURE_HTTP_HOSTS` | list | `[]` | - | Hosts reachable over plain `http` for: webhook deliveries. |
| `egress.webhooks.allowed_hosts` | `RUSTRYX_EGRESS_WEBHOOKS_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts (exact or `*.example.com`) may be targets of: webhook deliveries. |
| `egress.webhooks.allowed_ports` | `RUSTRYX_EGRESS_WEBHOOKS_ALLOWED_PORTS` | list of integers | `[443]` | - | Destination ports for: webhook deliveries. |
| `egress.webhooks.allow_private_cidrs` | `RUSTRYX_EGRESS_WEBHOOKS_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed despite the SSRF filter for: webhook deliveries. Loopback and metadata addresses are refused. |
| `egress.scanners.allowed_schemes` | `RUSTRYX_EGRESS_SCANNERS_ALLOWED_SCHEMES` | list | `["https"]` | - | Schemes for: http scanner adapters and data set downloads. Must include `https`. |
| `egress.scanners.allow_insecure_http_hosts` | `RUSTRYX_EGRESS_SCANNERS_ALLOW_INSECURE_HTTP_HOSTS` | list | `[]` | - | Hosts reachable over plain `http` for: http scanner adapters and data set downloads. |
| `egress.scanners.allowed_hosts` | `RUSTRYX_EGRESS_SCANNERS_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts (exact or `*.example.com`) may be targets of: http scanner adapters and data set downloads. |
| `egress.scanners.allowed_ports` | `RUSTRYX_EGRESS_SCANNERS_ALLOWED_PORTS` | list of integers | `[443]` | - | Destination ports for: http scanner adapters and data set downloads. |
| `egress.scanners.allow_private_cidrs` | `RUSTRYX_EGRESS_SCANNERS_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed despite the SSRF filter for: http scanner adapters and data set downloads. Loopback and metadata addresses are refused. |
| `egress.probe.allowed_schemes` | `RUSTRYX_EGRESS_PROBE_ALLOWED_SCHEMES` | list | `["https"]` | - | Schemes for: the certificate fetch helper. Must include `https`. |
| `egress.probe.allow_insecure_http_hosts` | `RUSTRYX_EGRESS_PROBE_ALLOW_INSECURE_HTTP_HOSTS` | list | `[]` | - | Hosts reachable over plain `http` for: the certificate fetch helper. |
| `egress.probe.allowed_hosts` | `RUSTRYX_EGRESS_PROBE_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts (exact or `*.example.com`) may be targets of: the certificate fetch helper. |
| `egress.probe.allowed_ports` | `RUSTRYX_EGRESS_PROBE_ALLOWED_PORTS` | list of integers | `[443]` | - | Destination ports for: the certificate fetch helper. |
| `egress.probe.allow_private_cidrs` | `RUSTRYX_EGRESS_PROBE_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed despite the SSRF filter for: the certificate fetch helper. Loopback and metadata addresses are refused. |
| `egress.smtp.allowed_hosts` | `RUSTRYX_EGRESS_SMTP_ALLOWED_HOSTS` | list | `[]` | - | When not empty, only these hosts may be the mail relay. |
| `egress.smtp.allowed_ports` | `RUSTRYX_EGRESS_SMTP_ALLOWED_PORTS` | list of integers | `[465, 587]` | - | Mail relay ports (465 and 587 by default). |
| `egress.smtp.allow_private_cidrs` | `RUSTRYX_EGRESS_SMTP_ALLOW_PRIVATE_CIDRS` | list | `[]` | - | Internal CIDRs allowed for an internal mail relay. Loopback and metadata addresses are refused. |
| `egress.proxy.allowed_hosts` | `RUSTRYX_EGRESS_PROXY_ALLOWED_HOSTS` | list | `[]` | - | Hosts an administrator may configure as the outbound proxy. Empty refuses the proxy setting. |

<!-- END GENERATED KEY REFERENCE -->
