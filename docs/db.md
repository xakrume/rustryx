# Database layer (`rustryx-db`)

`rustryx-db` implements every store port of `rustryx-core` on PostgreSQL 16 with `sqlx`, owns
the schema and its migrations, and provides the PostgreSQL based lock backend and cache
invalidation bus. It contains no business rules. What the schema can enforce cheaply against a
buggy caller (uniqueness, role nesting cycles, read-only built-ins, append-only audit) it
enforces; everything else is the services' job.

## 1. Contents

| Module | What it is |
|---|---|
| `config`, `pool` | `PgConfig`, `connect`, `connect_lazy`, `ping` |
| `migrate` | `migrate(url)`, `schema_status(pool)`, `SchemaStatus`, `expected_schema_version()` |
| `repos::*` | `PgRepositoryRepo`, `PgBlobStoreRepo`, `PgBlobRepo`, `PgComponentRepo`, `PgAssetRepo`, `PgUserRepo`, `PgRoleRepo`, `PgPrivilegeRepo`, `PgSelectorRepo`, `PgTokenRepo`, `PgIdpMappingRepo`, `PgLoginAttemptRepo` |
| `store` | `PgStore`: all of the above plus audit, job leases and system config over one pool |
| `audit`, `maintenance` | `PgAuditSink`, partition maintenance, retention, login counter purge |
| `lock` | `PgLockBackend` (`LockBackend` on advisory locks) |
| `invalidation` | `PgInvalidationBus` (`InvalidationBus` on `LISTEN`/`NOTIFY`) |
| `lease` | `PgJobLeases`: durable queues with `FOR UPDATE SKIP LOCKED` (no port in core) |
| `bootstrap` | `bootstrap_admin(pool, username, password_hash)` |
| `sealer` | `SecretSealer`, `AeadSealer` for upstream credentials at rest |
| `system_config` | `PgSystemConfig`: small runtime key/value settings |
| `error` | `map_err`: the one place that turns `sqlx::Error` into `StoreError` |

Wiring in the composition root:

```rust
let pool = rustryx_db::connect(&PgConfig::new(pooled_url)).await?;
let sealer = Arc::new(AeadSealer::new(KeyEntry::from_hex("2026-10", &key)?, retired_keys)?);
let store = PgStore::new(pool.clone(), sealer);
let repositories: Arc<dyn RepositoryRepo> = Arc::new(store.repositories.clone());
let locks = PgLockBackend::new(&direct_url)?;           // direct URL, never the pooler
let bus = store.invalidation_bus(direct_url.clone());   // direct URL for LISTEN
```

`/readyz` is `ping(&pool)` plus `schema_status(&pool)`: ready when the ping succeeds and
`status.is_serving()`. Cache the pair for about two seconds so probes do not load the pool.

## 2. Connection topology

| Use | URL | Why |
|---|---|---|
| Pooled queries (`connect`) | pooler or server | one statement or one transaction per call, safe in transaction mode |
| `migrate` | direct | the migration lock is a session advisory lock |
| `PgLockBackend` | direct | a session advisory lock lives as long as its session, see section 8 |
| `PgInvalidationBus::subscribe` | direct | `LISTEN` is bound to one session |
| `PgInvalidationBus::publish` | pool | one `pg_notify` statement |

TLS is selected by the URL (`sslmode=verify-full&sslrootcert=...`). sqlx uses rustls with the
`aws-lc-rs` provider and the webpki root set; there is no OpenSSL. The crate does not forbid a
plaintext URL because `migrate(url)` and the lock backend take bare URLs; the configuration
layer must reject anything weaker than `verify-full` outside development (SEC-26).

Pool defaults follow SRE-27: 20 connections per node, 5 s acquire timeout, 30 s
`statement_timeout` (set per connection with `set_config`, which works through poolers), 1 s
`ping`. The sum of `max_connections` over all nodes plus the lock and listen connections (one
per held lock and one per subscription) plus a reserve must stay below the server limit.

## 3. Schema

Migrations live in `crates/rustryx-db/migrations`, are embedded with `sqlx::migrate!` and are
forward-only. `build.rs` makes cargo rebuild when a file is added.

| Migration | Content |
|---|---|
| 0001 core | `pg_trgm`, `system_config`, `schema_contract` |
| 0002 storage | `blob_stores`, `repositories`, `blobs`, `components`, `assets`, orphan tracking trigger |
| 0003 identity | `users`, `content_selectors`, `privileges`, `roles`, `role_privileges`, `role_roles` (cycle guard), `user_roles`, `user_tokens`, `idp_group_mappings`, `login_attempts` |
| 0004 rbac seed and guards | built-in roles and privileges, then the read-only triggers |
| 0005 audit | `audit_events` partitioned by month, append-only triggers, partition functions |
| 0006 jobs | `job_leases` |

Invariants and what holds them:

| Invariant | Held by |
|---|---|
| Repository name unique | `uq_repositories_name` |
| A proxy has a proxy config, a group has a group config, hosted has neither | `ck_repositories_type_config` |
| A blob store or blob in use is not deleted | foreign keys with `ON DELETE RESTRICT` |
| A component coordinate is unique, NULL group and NULL version included | `UNIQUE NULLS NOT DISTINCT (repository_id, format, group_id, name, version)` (PostgreSQL 15+) |
| `(repository_id, path)` unique, byte ordered | `uq_assets_repo_path`, `path COLLATE "C"` |
| A blob is deleted only after it was unreferenced for the minimum age | `blobs.orphaned_at`, maintained by trigger `assets_blob_ref` |
| Username unique across all sources, case insensitively | unique index on `lower(username)` |
| External identity is `(source, issuer, subject)` in separate columns | `uq_users_external`, `ck_users_oidc_identity` |
| Only local accounts have a password hash, and it is a hash | `ck_users_hash_only_local`, `ck_users_password_hash_format` (argon2 PHC or bcrypt only) |
| Token hash is 64 lowercase hex characters, unique | `user_tokens` check and `uq_user_tokens_hash` |
| Role nesting has no cycle, also under concurrent writes | trigger `role_roles_no_cycle` with a global advisory transaction lock |
| Built-in roles and privileges are read-only | triggers `*_builtin_guard` (SQLSTATE `RX002`) |
| Content selector expression is at most 1 KiB | check constraint |
| Audit rows are never updated, deleted or truncated | trigger `audit_events_append_only` (SQLSTATE `RX003`) |
| One queued or running job per `(queue, dedupe_key)` | partial unique index |

Custom SQLSTATEs: `RX001` role nesting cycle, `RX002` built-in row is read-only, `RX003` audit
log is append-only. `error.rs` maps them to `StoreError::Conflict`.

No user and no password is seeded. The built-in roles are `nx-admin`, `nx-anonymous` and
`nx-developer` (names from `rustryx_core::identity`). `nx-anonymous` holds browse and read on
all repositories; the rbac crate must still require the global switch and the repository flag,
it must not rely on the role being empty. `nx-developer` holds browse, read and write.

### Built-ins are owned by migrations

`create` on roles and privileges always stores `builtin = false`, whatever the struct says. A
later migration that has to change a built-in role disables the guard inside its own
transaction (`ALTER TABLE role_privileges DISABLE TRIGGER role_privileges_builtin_guard`) and
enables it again before the end of the file.

## 4. Migrations and expand/contract

`rustryx migrate` calls `migrate(&direct_url)`. It opens a direct connection, takes
`pg_advisory_lock`, applies pending migrations (each in its own transaction), releases the lock
and returns which migrations THIS call applied. Concurrent callers are serialised; the ones that
wait find nothing left and report an empty list. `serve` never migrates.

`schema_status(pool)` compares the newest applied version with `expected_schema_version()`:

| Status | Meaning | Ready |
|---|---|---|
| `Current` | schema equals what the binary expects | yes |
| `OlderCompatible { applied, expected }` | schema is newer than this binary by expand migrations only; this binary is release N-1 during a rolling update | yes |
| `Behind { applied, expected }` | `rustryx migrate` has not run, or a migration is half applied | no (503) |
| `Ahead { applied, expected }` | a contract migration removed something this binary needs | no (503) |

The "N-1" of README section 8.2 is the binary: the Helm hook runs the new `migrate` before the
new pods start, so for a while pods of the previous release run against schema N. They must stay
ready, or the rollout takes the service down.

Rules for migration authors:

1. Expand first: add tables, columns (nullable or with a default) and indexes. Never drop,
   rename or tighten anything the previous release reads.
2. Contract in a later release, once no running binary needs the old shape. A contract migration
   ends with `UPDATE schema_contract SET min_binary_schema = <its own version>`. That is what
   turns older binaries not-ready instead of letting them fail on queries.
3. Never edit an applied file: `migrate` fails on a checksum mismatch, deliberately.
4. A `-- no-transaction` file can leave a failed row behind; `schema_status` then reports
   `Behind` at the version before it until an operator repairs it.

`Migrator::run_direct` (hidden in the sqlx docs, used by sqlx itself) is called instead of
`Migrator::run`, because the future of `run` is not provably `Send` ("implementation of
`Acquire` is not general enough"). `Cargo.lock` pins sqlx and the migrate tests exercise the
call.

## 5. Queries, errors and pagination

All statements are string literals or built with `QueryBuilder`; every value is a bind
parameter. sqlx 0.9 enforces this at compile time: a non-`'static` SQL string does not compile
unless wrapped in `AssertSqlSafe`, and a test fails the build if library code ever uses that
wrapper or `raw_sql`.

`error::map_err` is the only conversion from `sqlx::Error`:

| Cause | `StoreError` |
|---|---|
| unique, foreign key, check, not null violation | `Conflict` with a static text per constraint |
| `RX001`, `RX002`, `RX003` | `Conflict` ("role nesting cycle", "built-in entity is read-only", "audit log is append-only") |
| pool timeout, pool closed, I/O, TLS, connection class 08, shutdown, `statement_timeout`, too many connections | `Unavailable` |
| deadlock, serialization failure | `Unavailable` ("transaction conflict, retry") |
| row not found | `NotFound` |
| anything else | `Backend { context, source }` where `source` holds SQLSTATE and the server's primary message only |

Conflict texts never contain the offending value: PostgreSQL's own message
(`Key (username)=(alice) already exists`) is not copied. The `Backend` source drops the detail
and hint fields and the statement. Parse errors of a connection URL report a fixed message,
because the parser's text can contain the password.

Absent target semantics: `NotFound` for admin facing entities (repository, blob store, user,
role, privilege, selector, mapping, token revoke, component), idempotent success for GC facing
calls (`BlobRepo::delete`, `touch_*`) and for `delete_by_path`, which returns `None`.

Pagination is keyset. The token is `base64url("<kind>:<last key>")`: it carries a position, not
a permission (filters are applied in `WHERE` regardless), so it is not signed. Orders: users and
components by id (UUIDv7, creation order), assets by path in byte order. The limit is clamped
to 1..=1000. `StoreError` has no invalid-input variant, so a malformed token is a `Conflict`
("invalid page token"); restarting from page one would make a client with a bad token loop.

## 6. Concurrency design

* `AssetRepo::publish` is one transaction: insert the blob row, `SELECT .. FOR UPDATE` the asset
  by path, then `UPDATE` or `INSERT .. ON CONFLICT DO NOTHING`, retrying into the update branch
  if another publisher won the insert. Concurrent publishers of one path each report exactly
  the blob they displaced; a failure anywhere rolls the blob row back with the rest.
* `ComponentRepo::find_or_create` reads first and inserts with `ON CONFLICT DO NOTHING`, so the
  common case is one indexed read and no write, and concurrent creators converge on one row.
* Login counters are one `INSERT .. ON CONFLICT DO UPDATE` with the increment inside the
  statement. Sixty concurrent failures yield counts 1 to 60 exactly once each.
* The role graph is serialised by one advisory transaction lock, taken as the FIRST statement of
  every role write. The cycle trigger takes the same lock, but a writer that had already
  updated its role row and then waited for the lock deadlocked against another writer in
  testing; one lock order removes the class.
* Trap: a column of a unique index in the `SET` list of an `UPDATE` makes PostgreSQL take the
  row lock as a KEY update even when the value does not change. That lock blocks the foreign key
  checks of concurrent inserts that point at the row. Role, privilege and selector updates
  therefore keep `name` out of the `SET` list and rename in a second statement only when it
  changed (measured: an unchanged `SET name = name` blocked a concurrent insert for the whole
  transaction).
* Every RBAC relevant write queues its `NOTIFY` inside its own transaction, so listeners hear it
  after commit only, and never for a change that was rolled back.

## 7. Credentials at rest

`PgStore::new` requires a `SecretSealer`. Upstream passwords of proxy repositories never reach
`proxy_config` in clear text: the field holds an envelope `rx1.<key id>.<base64url(nonce ||
ciphertext || tag)>` produced with AES-256-GCM. The associated data is a purpose string plus
the repository id, so an envelope copied to another row does not open. The key ring has one
current key and any number of retired keys; `PgRepositoryRepo::reseal_upstream_secrets` rewrites
every envelope under the current key (compare-and-set per row), after which a retired key can be
dropped. A KMS or Vault Transit backed sealer can implement the trait without touching this
crate; the in-crate sealer has one level of keys, not DEK plus KEK.

Other secrets: API tokens are stored as the SHA-256 hex only; password hashes only as argon2
PHC or bcrypt strings (anything else is refused by a constraint, so passing a password instead
of its hash fails); `bootstrap_admin` takes a hash and creates the first administrator only when
no user holds `nx-admin`, serialised by an advisory transaction lock.

Audit grants. The migrating role owns the tables. The application role should hold no more on
the audit table than this (SEC-41); the triggers above are the second line, and they also stop
the owner from rewriting rows by accident:

```sql
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO rustryx_app;
REVOKE UPDATE, DELETE, TRUNCATE ON audit_events FROM rustryx_app;
```

Inserts and reads through the parent table need privileges on the parent only; the monthly
partitions need no grants of their own. Creating and dropping partitions needs ownership of the
parent, so with a non-owner application role `run_audit_maintenance` must run with the owner
credentials (for example a CronJob using the migrate credentials), and the sink's on-demand
partition creation (section 10) will fail with a backend error if the scheduled job ever falls
behind. With a single owner role for everything, no extra step is needed.

## 8. Locks (`PgLockBackend`)

One dedicated direct connection per held lock, `pg_try_advisory_lock(key)` where the key is the
first eight bytes of SHA-256 over `rustryx:lock:<name>`. The server drops the lock with the
session, so a dead node frees its locks without any timer.

TTL is emulated so that jobs behave the same on this backend and on Redis: the guard is valid
for `ttl` after acquisition and after each successful `renew`. A holder that misses renewals
for longer than `ttl` reports `is_lost() == true`, `renew()` returns `false` and the connection
is closed, which frees the lock for another node. `renew` also checks `pg_locks` for the
session's own lock, so a dead connection or a lock lost any other way returns `false`; a query
that hangs past the deadline counts as loss. A guard dropped without `release` frees the lock
when its connection closes. `try_acquire` returns `Ok(None)` when someone else holds the lock
(the connection is closed again at once) and `Unavailable` when the server cannot be reached,
so a caller never starts a singleton job on an unavailable backend.

## 9. Invalidation bus (`PgInvalidationBus`)

Channel `rustryx_rbac`; payloads `rbac`, `user:<uuid>`, `reset`. An unknown payload decodes to
`Reset`: dropping all caches is always safe, ignoring a message is not. `subscribe()` is lazy and
pull based (no internal buffer, so a slow consumer cannot lose events silently). The first item
after every (re)connection is `Reset`, yielded only after `LISTEN` succeeded, so no change falls
between "caches dropped" and "listening". The listener uses `try_recv`, which reports a lost
session instead of reconnecting silently. While the database is down the stream keeps retrying
with a capped backoff and does not end. Publishing reaches every subscriber including the
publishing node.

## 10. Audit partitions

`audit_events` is `PARTITION BY RANGE (event_time)` with UTC month boundaries and partitions
named `audit_events_yYYYYmMM`. The first migration creates the current month and two ahead.
A scheduler calls `run_audit_maintenance` (under a distributed lock, though the SQL functions
also serialise themselves): lookahead first, then retention (default 90 days, whole months only).
Retention drops partitions, it does not archive them; archive first if the policy requires it.
Only partitions with a generated name are candidates, so a partition attached by hand (a legal
hold) is never dropped. If an event arrives for a month with no partition, the sink creates it
and retries once rather than lose the event. Also available: `purge_login_attempts`.

## 11. ADRs

**ADR-DB-1: runtime `sqlx::query` instead of `query!` with a committed `.sqlx`.** The task
preferred compile-time checked queries when practical. They are not here: (a) `.sqlx` must match
the schema exactly, and a stale file breaks `cargo clippy --workspace --locked` for every crate
that depends on this one while many people edit concurrently; (b) CI would need either a live
database at build time or a `cargo sqlx prepare --check` step with its own container; (c) half
of the interesting statements are dynamic (`QueryBuilder`) or map text columns onto domain enums
through custom conversions, so the macros would cover a minority and need type overrides on most
columns. What replaces the compile time check: every statement is executed by an integration
test against PostgreSQL 16 (type or column errors fail there), values are bound parameters, and
sqlx 0.9 rejects non-literal SQL at compile time. Revisit if a CI database becomes available.

**ADR-DB-2: text columns with CHECK instead of PostgreSQL enum types.** Adding a value to a
CHECK is an ordinary transactional migration and works with expand/contract; `ALTER TYPE .. ADD
VALUE` has restrictions inside transactions. Repository and component `format` is only
shape-checked, so phase 2 formats need no migration; the domain enum rejects unknown labels on
read.

**ADR-DB-3: case insensitive global usernames.** `lower(username)` is unique across sources.
`Admin` next to `admin` is the account confusion the cross-source conflict rule exists to
prevent. `find_by_username` uses the same expression. The auth crate should fold names the same
way.

**ADR-DB-4: `blobs.orphaned_at`.** GC must not delete a blob that was replaced a second ago:
a client may still be streaming it, and its creation time says nothing about that. A trigger
records when the last asset reference went away (cascades included) and `list_unreferenced`
compares that instant with the minimum age.

**ADR-DB-5: `schema_contract.min_binary_schema`.** Gives "schema N-1 is permitted by
expand/contract" an explicit mechanism: a binary keeps serving while the schema is ahead as
long as no contract migration raised the floor above its version.

**ADR-DB-6: sealing in this crate.** SEC-25 forbids clear text upstream credentials in the
dump. The port hands this crate a `ProxyConfig` with a `Secret`, so the sealing has to happen
here. The key source is injected, which keeps KMS integration out of this crate.

**ADR-DB-7: PostgreSQL 15+ features.** `UNIQUE NULLS NOT DISTINCT` for coordinates, `MERGE`
is not used. The target is PostgreSQL 16.

**ADR-DB-8: rustls with aws-lc-rs.** One crypto provider across the workspace (`ldap3` and
`reqwest` already use it); no `openssl-sys`.

## 12. Tests

`cargo test -p rustryx-db` needs Docker (testcontainers 0.28, image `postgres:16-alpine`). One
container serves the whole test binary. It is started on a runtime owned by a dedicated thread
that outlives the per-test runtimes (the handle and its Docker client must not die with the
first test). testcontainers 0.28 has no reaper process: a container is removed when its handle
drops, and a static handle never does. The harness therefore limits the container's life
itself: it runs `(sleep N; kill 1) &` before `exec`ing the image entrypoint and asks Docker to
auto-remove it, so the server shuts down and disappears after 20 minutes at most
(`RUSTRYX_DB_TEST_CONTAINER_TTL_SECS` overrides it; keep it above the longest test run). The
migrated schema is built once as a template database and every test gets
`CREATE DATABASE .. TEMPLATE` of it, so tests are isolated and cheap; the empty-database variant
is used by the migration tests. `pg_locks` and `pg_stat_activity` are cluster wide: tests that
look up a session pid filter by database.

Unit tests cover the pure parts (cursor tokens, error mapping, event payloads, schema
classification, sealing). Test names starting with `must_not_` are the negative requirements:
no SQL text, row value or password in errors, no `AssertSqlSafe`, no plaintext password, no
default admin, no edit of built-ins, no update or delete of audit rows, no sealed secret that
opens on another row, no GC of a freshly replaced blob, no concatenated issuer and subject, no
link by email.

## 13. Known gaps and open items

* `cleanup_policy_ids` and `routing_rule_id` on repositories are stored as bare ids. There are
  no tables or ports for cleanup policies and routing rules in core yet.
* No audit read port in core: export (NDJSON) and queries need one.
* `StoreError` has no invalid-input variant: malformed page tokens and out-of-range numbers are
  reported as `Conflict`.
* `PgJobLeases` and `PgSystemConfig` are inherent APIs because core has no ports for them.
* `login_attempts` rows are purged by `purge_login_attempts`, which a scheduler must call.
* Audit partition privileges for the application role are an operational step (section 7).
