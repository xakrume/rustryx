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
| `repos::*` | `PgRepositoryRepo`, `PgBlobStoreRepo`, `PgBlobRepo`, `PgComponentRepo`, `PgAssetRepo`, `PgUserRepo`, `PgRoleRepo`, `PgPrivilegeRepo`, `PgSelectorRepo`, `PgTokenRepo`, `PgIdpMappingRepo`, `PgLoginAttemptRepo`, `PgRoutingRuleRepo` |
| `store` | `PgStore`: all of the above plus audit write and read, job leases, system config and revoked tokens over one pool |
| `audit`, `audit_read`, `maintenance` | `PgAuditSink`, `PgAuditRead` (`AuditReadRepo`: filtered keyset query and the export stream), partition maintenance, retention, login counter purge |
| `lock` | `PgLockBackend` (`LockBackend` on advisory locks) |
| `invalidation` | `PgInvalidationBus` (`InvalidationBus` on `LISTEN`/`NOTIFY`) |
| `lease` | `PgJobLeases` (`JobLeaseRepo`): durable queues with `FOR UPDATE SKIP LOCKED`, batch enqueue with the outbox cursor hand-off |
| `revoked` | `PgRevokedTokens` (`RevokedTokenRepo`): revoked internal JWT ids with expiry purge |
| `outbox` | the in-transaction insert of outbox events and the consumer cursor compare and set (crate internal, plus the channel name); the `OutboxRepo` port itself is not here yet |
| `bootstrap` | `bootstrap_admin(pool, username, password_hash)` |
| `sealer` | `SecretSealer`, `AeadSealer` for upstream credentials and sealed settings at rest |
| `system_config` | `PgSystemConfig` (`SystemConfigRepo`): runtime settings with sealed fields and a revision |
| `error` | `map_err` and `map_mutation_err`: the one place that turns `sqlx::Error` into `StoreError` and `MutationError` |

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
| 0007 wave 2 foundation | `projects`, `repositories.project_id` with its triggers, uploader and pull time columns, `audit_events.actor_kind` and `project_id`, settings revision (`system_config_revision_seq`, trigger `system_config_bump`), `revoked_tokens`, `node_heartbeats`, `event_outbox` (partitioned) with `outbox_cursors` and its partition functions, `task_definitions`, `task_runs`, `task_run_log` |
| 0008 routing rules | `routing_rules` and the foreign key `repositories.routing_rule_id` |

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
| `allow_anonymous` of a repository in a project follows the project visibility, for every writer | triggers `repositories_project_sync` and `projects_visibility_sync` |
| A project that has repositories is not deleted | `repositories.project_id` is `ON DELETE RESTRICT` |
| Every settings write gets a new revision from one sequence, also from an N-1 writer | trigger `system_config_bump` |
| One queued or running task run per `(kind, scope_key)` | partial unique index `uq_task_runs_active` |
| An outbox partition is not dropped while a consumer has not handled an event in it | `drop_old_outbox_partitions` checks `(tx_id, seq) > cursor` itself |
| A routing rule in use is not deleted; a rule has 1 to 32 patterns of 1 to 256 bytes | `fk_repositories_routing_rule` (`RESTRICT`), `ck_routing_rules_patterns` |

Custom SQLSTATEs: `RX001` role nesting cycle, `RX002` built-in row is read-only, `RX003` audit
log is append-only. `error.rs` maps them to `StoreError::Conflict`. The policy guards of later
migrations use `RX004` immutable, `RX005` held, `RX006` quota exceeded, `RX007` read only mode and
`RX008` scope violation; their mapping is in section 5 and the DETAIL convention in ADR-DB-10.

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
5. Version numbers are strictly increasing, not contiguous: the streams of wave 2 own ranges
   (0020 to 0139, wave2-design.md section 2.7) and `sqlx` applies every embedded version that is
   not recorded yet, so a gap is harmless. A unit test pins "ordered, unique, forward only".

Migration 0007 is an expand migration and leaves `schema_contract.min_binary_schema` at 1: a
binary of the previous release (schema 6) stays ready against schema 8
(`SchemaStatus::OlderCompatible`). The test `a_binary_of_the_previous_release_keeps_working_against_the_new_schema`
runs the phase 1 statements verbatim against the new schema (no new column named, the new
triggers firing around them) and checks that the floor is untouched. What an N-1 binary cannot do
is use the new data: it rewrites `proxy_config` without the four wave 2 fields, which only matters
once an operator has set one (gated by the feature level), and `PgSystemConfig::set` of the old
build writes a value without sealing, which is why settings with secrets are written only after
the gate of the settings stream opens.

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
| `RX004` to `RX008` from `map_err` (the phase 1 methods) | `Conflict` with the words of the matching `PolicyViolation` ("item is immutable", "item is under a hold", "quota exceeded", "read only mode", "scope violation") |
| `RX004` to `RX007` from `map_mutation_err` (the typed methods) | `MutationError::Violation(Immutable { rule } / Held { hold } / QuotaExceeded(..) / ReadOnly)`; a guard whose DETAIL is missing or malformed degrades to the `Conflict` above, never to a backend error |
| `RX008` from `map_mutation_err` | `MutationError::Store(Conflict("scope violation"))`: there is no `PolicyViolation` for it |
| bad page token, number out of range, oversized event, invalid pointer, queue name or key out of bounds | `Invalid` (never `Conflict`) |
| pool timeout, pool closed, I/O, TLS, connection class 08, shutdown, `statement_timeout`, too many connections | `Unavailable` |
| deadlock, serialization failure | `Unavailable` ("transaction conflict, retry") |
| row not found | `NotFound` |
| anything else | `Backend { context, source }` where `source` holds SQLSTATE and the server's primary message only |

Conflict texts never contain the offending value: PostgreSQL's own message
(`Key (username)=(alice) already exists`) is not copied. The `Backend` source drops the detail
and hint fields and the statement. Parse errors of a connection URL report a fixed message,
because the parser's text can contain the password.

Absent target semantics: `NotFound` for admin facing entities (repository, blob store, user,
role, privilege, selector, mapping, token revoke, component, routing rule, the repository of a
publish, the blob of a link), idempotent success for GC facing calls (`BlobRepo::delete`,
`delete_if_unreferenced` answering `false`, `touch_downloaded`, `record_downloads`) and for
`delete_by_path` and `delete_by_path_checked`, which return `None`. `touch_fresh` of a missing
asset is `NotFound`; `set_password_hash_if` of a missing user is `NotFound` and of an account
without a hash is `false`.

Pagination is keyset. The token is `base64url("<kind>:<last key>")`: it carries a position, not
a permission (filters are applied in `WHERE` regardless), so it is not signed. Orders: users and
components by id (UUIDv7, creation order), assets by path in byte order, audit events newest
first by `(event_time, id)` (the token key is `<microseconds>:<uuid>`). The limit is clamped to
1..=1000. A malformed token, a token of another listing and an out of range number are
`StoreError::Invalid` ("invalid page token", mapped to 400 by the HTTP layer); restarting from
page one would make a client with a bad token loop, and a `Conflict` would claim a state clash
that does not exist.

## 6. Concurrency design

* `AssetRepo::publish_with` is one transaction, and `publish` is the provided wrapper over it
  (a violation flattened to `Conflict`). Order of work: validate the options (an event over
  16 KiB is `Invalid` before anything is opened), `SELECT .. FOR SHARE` the repository row (write
  policy and quota are read from it), refuse at once when the policy is `Deny`, insert the blob
  row, `SELECT .. FOR UPDATE` the asset by path, check the mode and the policy against what that
  locked read found, then `UPDATE` or `INSERT .. ON CONFLICT DO NOTHING` (retrying into the update
  branch if another publisher won the insert), check the repository quota, write the outbox
  events, commit. Every refusal returns before the commit, so no asset, blob row or event is left.
  Because the existing row is read under its lock, `PublishMode::CreateOnly` and `allow_once`
  hold across nodes: of twelve concurrent create-only publishers of one path exactly one wins.
  Concurrent publishers of one path each report exactly the blob they displaced, and the
  displaced blob is returned only when it lost its last reference (with `link` two assets can
  share a blob, and handing out a live one "for GC" would invite deleting served content).
* `AssetRepo::link` publishes by reference. Lock order everywhere: repository row (share), asset
  row (`FOR UPDATE`), blob row (`FOR NO KEY UPDATE`). The `assets_blob_ref` trigger touches the
  blob rows of a switch in the same order, and the GC sweep takes a blob row only, so there is no
  cycle. `FOR NO KEY UPDATE` and not `FOR SHARE` on the blob: two links of one orphaned blob would
  both hold a share lock and both need the row lock of that trigger, a deadlock the test
  `two_links_of_one_orphaned_blob_do_not_deadlock` pins down.
* Garbage collection order, decided by the lead: ROW first under a row lock, OBJECT second.
  `BlobRepo::delete_if_unreferenced` locks the row `FOR UPDATE`, re-checks age (`orphaned_at`, or
  `created_at` for a never referenced blob) and references in a separate statement (a fresh
  `READ COMMITTED` snapshot, so it sees a link that committed while it waited for the lock; a
  single `DELETE .. WHERE NOT EXISTS` would not), deletes the row and commits, and only an
  answer of `true` lets the caller delete the object. A link that wins the race makes the sweep
  answer `false`; a sweep that wins makes the link `NotFound`; so a link never points at a
  deleted object. The test `must_not_leave_a_link_pointing_at_a_deleted_object_under_concurrent_gc`
  models the object store as a set, runs 40 rounds of three linkers against one sweep with a
  staggered start, and checks both outcomes and the invariant. The foreign key `RESTRICT` is
  the backstop: without the lock the failure would be a `Conflict` from the sweep, not a dangling
  asset. `BlobRepo::delete` stays for tests and tooling and is documented as not safe for GC.
* The repository quota is the phase 1 sum, checked inside the publish transaction on the actual
  size: usage is the sum over DISTINCT blobs of the repository, the delta of the write is the new
  blob's size when no other asset of the repository holds it minus the old blob's size when the
  replaced blob has no holder left, and a write that does not grow the repository (`delta <= 0`)
  is never refused. A repository with a quota takes a transaction advisory lock named after it
  between reading and writing, so twenty concurrent publishes into a quota of 100 bytes with ten
  byte blobs succeed exactly ten times; repositories without a quota do not serialize. Project
  quotas and the counters belong to the lifecycle stream and replace this sum.
* Outbox events ride in the transaction of the write (`publish_with`, `link`,
  `delete_by_path_checked`, `delete_checked`): one `INSERT .. SELECT FROM unnest(..) WITH ORDINALITY
  ORDER BY ord`, so sequence numbers follow the order of the drafts, and one `pg_notify` of
  `rustryx_outbox` with an empty payload, delivered after commit only. A month without a partition
  (the maintenance job fell behind) is repaired inside the same transaction: the first insert runs
  in a savepoint, `ensure_outbox_partitions(now(), 0)` creates the partition, the insert repeats
  once. Failing a publish because housekeeping was late would be worse than a short DDL lock.
* Of the other `WriteOptions` fields only `mode`, `override_immutable` and `events` act in this
  crate. `actor` and `request_id` are not used: every `EventDraft` carries its own actor and
  request id, and the audit batch row of a lifecycle run is written by its caller through the
  sink.
* `WriteOptions::override_immutable` becomes `set_config('rustryx.override_immutable', 'on',
  true)`, written only when set and local to the transaction, so a pooled connection never
  carries the bypass into the next request (`must_not_carry_the_override_into_the_next_transaction_of_a_pooled_connection`
  runs on a one connection pool).
* `record_downloads` takes the row locks first, in ascending id order, with a materialized
  `SELECT .. FOR UPDATE .. ORDER BY id`, then updates assets and their components; two nodes
  flushing overlapping sets wait for each other instead of deadlocking. A stored time is rewritten
  only when it lags the new one by more than the write interval (default 60 s, a hot asset costs
  one write a minute). The counts are accepted and not used yet: `asset_downloads_daily` belongs
  to the lifecycle stream, which adds its insert here.
* `UserRepo::set_password_hash_if` is one `UPDATE .. WHERE password_hash = $old`; of twelve
  concurrent rehashes exactly one wins.
* `JobLeaseRepo::enqueue_batch` makes the cursor move first and the inserts second, in one
  transaction, and a lost compare and set returns 0 with nothing inserted: of ten consumers that
  read the same batch exactly one writes its rows (the atomic hand-off of the design). A consumer
  that never ran has no cursor row, which is the position `(0, 0)`; the first move creates the
  row, and only from that position.
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

Sealed settings. `SystemConfigRepo::put` seals the strings at the JSON pointers the caller names
and stores the envelope at the same pointer plus a top level `_sealed` array that lists the
pointers, so the row says by itself which fields are sealed and the N-1 binary reads it as opaque
JSON. The associated data is the key AND the pointer (`rustryx/system-config/seal/v1/<key>\0<pointer>`):
an envelope copied to another key or to another field of the same value does not open. A field
absent from a later write keeps its stored envelope, a JSON `null` clears it, a new string is
sealed afresh. `get` and `list_prefix` with `SecretView::Mask` remove the sealed fields and keep
the `_sealed` list (the API says "password set" from it, and a masked read never decrypts);
`Reveal` opens them and drops the list. `put` returns the masked view, so a write never echoes a
secret. `_sealed` is reserved: a caller value that has the key is `Invalid`.

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

Channel `rustryx_rbac`; payloads `rbac`, `user:<uuid>`, `reset`, `projects` and `config:<revision>`
(settings written through `SystemConfigRepo`, the revision being the one of the write or, for a
delete, a fresh one from the sequence). The trigger `projects_visibility_sync` sends `projects`
from SQL. A blob store edit sends `reset`: there is no finer event for the registry cache yet.
An unknown payload decodes to
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

`event_outbox` follows the same scheme (`ensure_outbox_partitions(now, lookahead)`, default two
months ahead, created by the migration; `drop_old_outbox_partitions(now, retention_days)`, default
30). The drop function adds a rule the audit one does not have: a partition that holds an event a
consumer has not handled (`(tx_id, seq) > (last_tx_id, last_seq)` for some row of
`outbox_cursors`) is kept, so "never drop past the slowest cursor" cannot be forgotten by a
caller. A consumer without a cursor row is not waited for. A dead consumer therefore pins its
partitions, which is what the lag alert is for.

`AuditReadRepo` reads the same table. `query` pages newest first and `export` streams oldest
first, both by `(event_time, id)`. The export runs in keyset batches of 500 rows, each a separate
statement, so a slow NDJSON consumer holds no pooled connection between batches and dropping the
stream ends the work; the price is that an export is not one snapshot (an event stamped before the
position already handed out is missed, a window of the clock skew between nodes). A row whose
kind or actor kind this build does not know is an error item that ends the stream, never a
skipped row: an incomplete audit trail must not look complete.

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

**ADR-DB-9: GC order, row first.** `BlobRepo::delete_if_unreferenced` locks the blob row, re-checks
age and references, deletes the ROW and commits; the caller deletes the OBJECT after `true`. With
`AssetRepo::link` an orphan can be referenced again, so deleting the object first could leave an
asset that points at nothing; deleting the row first can leave, after a crash, an orphan object
that `verify_blobs` finds. This reverses the phase 1 wording of `delete` ("object first"), which
stays for tests and tooling. Section 6 has the lock order and the race test.

**ADR-DB-10: typed policy guards by SQLSTATE and DETAIL.** A guard trigger raises `RX004`
(DETAIL: the rule id), `RX005` (DETAIL: the hold id), `RX006` (DETAIL:
`<project|repository>:<scope id>:<limit>:<used>:<requested>`) or `RX007` (no DETAIL). `map_mutation_err`
parses the DETAIL into a `PolicyViolation`; `map_err` ignores it and gives the phase 1 `Conflict`
the same words. The DETAIL is parsed and never copied, a missing or malformed one degrades to the
fixed `Conflict`, and the tests install stand-in triggers to prove the contract before the real
guards exist. `RX008` has no `PolicyViolation` and stays a `Conflict`.

**ADR-DB-11: repository quota by sum under an advisory lock, for now.** The design wants conditional
counter updates (`quota_usage`), which belong to the lifecycle migrations. Until the counters
are authoritative the commit check is the phase 1 sum, made exact by a per repository advisory
transaction lock that only repositories with a quota take. It scans the repository's blobs, which
is the cost the design accepts for the interim.

**ADR-DB-12: sealing metadata inside the settings value.** The `_sealed` pointer list lives in the
JSON of the row, not in a column, so migration 0007 stays as designed, an N-1 reader sees opaque
JSON, and a row copied anywhere carries its own description. The envelope is bound to key and
pointer, which is stricter than the design's `settings.<key>` purpose.

**ADR-DB-13: the audit export is keyset batches, not a cursor.** One long query would pin a pooled
connection for as long as the client downloads and ties the stream's lifetime to a transaction;
batches of 500 hold nothing between rows and cancel by dropping the stream.

**ADR-DB-14: outbox partitions repaired in the transaction, and a drop that respects cursors.** See
section 6 and section 10. The cursor check lives in `drop_old_outbox_partitions` so every caller
gets it.

**ADR-DB-15: `routing_rules` in migration 0008.** The foundation design keeps it out of 0007
(only the streams that reference a table share it) and gives no number to its owner, so it is
the next free one. `repositories.routing_rule_id` gets a foreign key that is `NOT VALID`: it is
enforced for every new and changed value, and the migration cannot fail on bare ids written
before the table existed (none of which can be a valid reference).

**ADR-DB-16: `BlobStoreRepo::update` publishes `Reset`.** No dedicated event exists and a registry
cache that rebuilds a store is the only consumer; the edit is rare, so dropping every cache is
cheap. Name and kind are identity (a mismatch is `Conflict`), and soft quota and group settings,
which have no column yet, are refused with `Invalid` instead of being dropped silently.

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

Unit tests cover the pure parts (cursor tokens, error mapping and the guard DETAIL parser, event
payloads, schema classification and numbering, sealing, JSON pointer handling of settings, the
write policy matrix, audit positions). The integration binary has one module per area; the new
ones are `publish_with_tests`, `link_gc_tests`, `guard_tests`, `downloads_tests`,
`foundation_tests` (migrations 0007 and 0008, N-1 statements, outbox partition functions),
`outbox_tests`, `audit_read_tests`, `revoked_tests`, `routing_tests`, `repository_wave2_tests`
(projects, proxy fields, blob store update, application grants) and `closed_pool_tests` (every
port method reports a closed pool as `Unavailable`). The guards of later migrations do not exist
yet, so `guard_tests` installs stand-in triggers that raise the documented SQLSTATEs.

Test names starting with `must_not_` are the negative requirements: no SQL text, row value, guard
text or password in errors, no `AssertSqlSafe`, no plaintext password, no default admin, no edit of
built-ins, no update or delete of audit rows, no sealed secret that opens on another row, key or
field, no sealed setting in clear text, no GC of a freshly replaced blob, no link that points at a
deleted object under concurrent GC, no replacement of an existing path in create only mode (and no
second winner among concurrent create only publishers), no override of immutability that outlives
its transaction, no wake-up for a refused write, no batch written after losing the outbox cursor,
no drop of an outbox partition with unhandled events, no purge of a revoked token id before its
expiry (and no shortened expiry), no row of another project in a project filtered audit read
(viewer pages and export), no concatenated issuer and subject, no link by email. Mutating the
code under four of them (create only check, purge boundary, expiry maximum) makes them fail.

## 13. Known gaps and open items

* Not implemented here, by assignment: the ports of the other wave 2 streams, and of the
  foundation ones that have a table but no port yet: `OutboxRepo` (read side, lag, purge),
  `NodeRegistryRepo`, `TaskDefinitionRepo`, `TaskRunRepo`. Their tables exist (migration 0007)
  and the shared pieces they need are here (`outbox` insert and cursor compare and set).
* `cleanup_policy_ids` on repositories is still a bare id list: the table belongs to the
  lifecycle stream. `registry_id` of a proxy is stored and returned but not resolved: the
  `registries` table belongs to the distribution stream, so `remote_url` and `auth` are the
  repository's own until then.
* `BlobStoreDef::soft_quota` and `group` have no column yet (lifecycle and storage streams); a
  definition that carries them is refused with `Invalid`. The `blob_stores.kind` CHECK still lists
  `local` and `s3`; `Azure` and `Group` are mapped and refused by the constraint until the
  storage migration.
* Application privileges (`PrivilegeGrant::Application`) read back, but the privileges CHECKs do
  not list the kind: writing one is refused with a fixed `Conflict` until the migration of the
  `ApplicationPrivileges` feature.
* The immutability, hold and read only guards (`RX004`, `RX005`, `RX007`) and the project and
  quota counters are later migrations. `publish_with` already runs under them (they fire on its
  statements), enforces the repository quota with the phase 1 sum (ADR-DB-11) and does not look at
  project quotas.
* `link` requires the blob's store to equal the repository's store. A group backed repository
  (blob rows name the concrete member store) needs the membership check of the storage stream.
* `record_downloads` does not count downloads yet (`asset_downloads_daily` is the lifecycle
  stream's table).
* The settings delete takes a fresh revision for its announcement, but `current_revision` is a
  maximum over existing rows and can go down after a delete; a node must also reload on the
  `config:` event.
* `login_attempts` rows are purged by `purge_login_attempts`, which a scheduler must call, and
  so are the outbox partitions (`drop_old_outbox_partitions`) and the finished job rows
  (`purge_finished`); the new `purge_expired` of revoked tokens is one more scheduled call.
* The unique index of `event_outbox` is `(id, occurred_at)`, as every unique index of a
  partitioned table must contain the partition key: an event id is unique within its instant,
  not globally. Ids are `UUIDv7` made here.
* Audit partition privileges for the application role are an operational step (section 7); the
  same holds for the outbox partition functions, which need ownership of the parent.
