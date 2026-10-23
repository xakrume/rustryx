# rustryx-testkit

In-memory implementations of the `rustryx-core` ports, so that tests of `rbac`, `auth`,
`tasks`, the format crates and `api` run without PostgreSQL or S3. Add it as a dev dependency:

```toml
[dev-dependencies]
rustryx-testkit.workspace = true
```

Nothing here is for production: the crate is not a dependency of any binary.

Contents:

1. [What is in it](#1-what-is-in-it)
2. [Using `MemStore`](#2-using-memstore)
3. [Semantics](#3-semantics): the rules every port of the store follows, then one section per area.
4. [Where it is looser than PostgreSQL](#4-where-it-is-looser-than-postgresql) and where it is stricter.
5. [Fixtures](#5-fixtures)
6. [Testing failure paths](#6-testing-failure-paths)
7. [What is not here](#7-what-is-not-here)

## 1. What is in it

| Item | Implements | Notes |
|---|---|---|
| `MemStore` | every repository port of `rustryx_core::ports` (the list is below) | One value, all ports. Cheap `Clone`, clones share state. |
| `MemBlobStore` | `BlobStore`, `ResumableBlobStore` | Streaming writes, incremental SHA-1/SHA-256/MD5, ranges, `list_keys`, resumable uploads, a switchable fake presigner. |
| `FixedClock` | `Clock` | Moves only on `advance` and `set`. |
| `MemLockBackend` | `LockBackend` | TTL against tokio time or an injected clock. |
| `MemAuditSink` | `AuditSink`, `AuditReadRepo` | `events()` returns what was recorded; the same log answers `query` and `export`. |
| `MemInvalidationBus` | `InvalidationBus` | `tokio::sync::broadcast`; a lagging subscriber gets `Reset`. |
| `fixtures` | | Entities and helpers, see section 5. |

`MemStore` implements these ports (core revision 2, wave 2 complete):

| Area | Ports |
|---|---|
| Catalog and content | `RepositoryRepo`, `BlobStoreRepo`, `BlobRepo`, `ComponentRepo`, `AssetRepo` |
| Identity and access | `UserRepo`, `RoleRepo`, `PrivilegeRepo`, `SelectorRepo`, `TokenRepo`, `IdpMappingRepo`, `LoginAttemptRepo` |
| Cross-cutting | `RevokedTokenRepo`, `JobLeaseRepo`, `AuditReadRepo` and `AuditSink`, `RoutingRuleRepo`, `OutboxRepo`, `SystemConfigRepo`, `NodeRegistryRepo`, `TaskDefinitionRepo`, `TaskRunRepo`, `TrustedCertificateRepo` |
| Projects | `ProjectRepo`, `ProjectMemberRepo`, `ProjectAccessRepo`, `LabelRepo`, `RobotTokenRepo` |
| Lifecycle | `QuotaRepo`, `ImmutabilityRepo`, `LifecycleHoldRepo`, `CleanupPolicyRepo`, `RetentionPolicyRepo`, `LifecycleRepo`, `LifecycleRunRepo`, `GcRunRepo`, `DownloadStatsRepo` |
| Supply chain | `ScannerRepo`, `ScanRepo`, `CveAllowlistRepo`, `SecurityPolicyRepo`, `TrustRepo`, `SecurityHubRepo`, `ExportRepo`, `ScannerDataRepo` |
| Distribution | `RegistryRepo`, `ReplicationRepo`, `WebhookRepo`, `WebhookDeliveryRepo`, `NotificationRepo`, `PromotionRepo` |
| Storage | `BlobStoreMemberRepo`, `BlobMoveRepo`, `VerifyRunRepo`, `BlobStoreStatsRepo` |

The migration notes of core revision 2 name helper doubles `MemOutbox`, `MemTaskStore`,
`MemSystemConfig` and `MemGroupStore`. They are not separate types here: a standalone
outbox that shares no transaction with the content writes could not show the property that
matters (an event exists if and only if its change committed), so `MemStore` is the outbox,
the task store, the settings store and the group store. A test that needs only one of them
takes the corresponding trait object of the same store.

## 2. Using `MemStore`

`MemStore` implements many traits that share method names (`create`, `get`, `list`,
`delete`, `get_by_name`). A plain method call on a `MemStore` that has several of those
traits in scope does not compile (E0034). Do what the composition root does and go
through the trait object:

```rust
let store = MemStore::with_builtin_roles().with_default_blob_store();
let users: Arc<dyn UserRepo> = Arc::new(store.clone());
let roles: Arc<dyn RoleRepo> = Arc::new(store.clone());
```

Constructors and builders:

- `MemStore::new()` is empty.
- `MemStore::with_builtin_roles()` seeds `nx-admin`, `nx-developer`, `nx-anonymous` and
  one built-in privilege each (admin: `Admin` with all four actions; developer:
  `RepositoryView` on `*` with Browse, Read, Write; anonymous: the same with Browse,
  Read). The ids are constants (`builtin_role_id(name)`), so
  `fixtures::principal_with_roles(&["nx-developer"])` carries the id that exists in a
  seeded store and `RoleRepo::effective_privileges` resolves it.
- `.with_default_blob_store()` inserts `fixtures::blob_store_def()`, the definition that
  `fixtures::repository` points at through `fixtures::BLOB_STORE_ID`.
- `.with_clock(Arc<dyn Clock>)` makes `created_at`, `updated_at`, lease times, expiry
  checks and every other stamp of the store come from a `FixedClock`. Call it before
  cloning the store. Without it the store reads the system clock.
- `.with_invalidation_bus(bus)` publishes events after writes that other nodes must see:
  `InvalidationEvent::Rbac` for roles, privileges, selectors and mappings,
  `InvalidationEvent::User(id)` for role replacement, enable/disable, token validity,
  user deletion, token revocation and the membership of a user in a project,
  `InvalidationEvent::Projects` for project, group membership and attachment changes,
  `InvalidationEvent::Config(revision)` for settings and every write that bumps
  `security.revision`, and `InvalidationEvent::Reset` for changes of blob stores and
  registries (there is no narrower event, and a cache must not keep an adapter built from
  the old settings).
- `.with_audit_sink(sink)` and `audit_sink()`: the store is an `AuditSink` and an
  `AuditReadRepo` over one log. `audit_sink()` hands out the `MemAuditSink` the store reads
  from, so a service that records through its own sink and a test that reads through the
  port look at the same events.
- `set_unavailable(true)` makes every port call fail with `StoreError::Unavailable`, for
  every port above (login guard fail-closed paths, readiness). Clones share the switch.
- `set_read_only(enabled, reason)` writes the `system.read_only` row the way the system
  endpoint does. Every guard of the store reads that row.
- `insert_blob(record)` seeds a blob row no asset references, for garbage collection
  tests. Published blobs arrive through `AssetRepo::publish_with`.

## 3. Semantics

These follow the port documentation (`ports/repos.rs` and the other port modules). Where a
port is silent, this is what the double does.

### 3.1 Rules every port follows

- Duplicates (name, id, token hash, external identity) are `Conflict`. `update`, `delete`,
  `set_*` and `revoke` of a row that does not exist is `NotFound` for admin facing entities
  (repository, blob store, user, role, privilege, selector, mapping, component, token,
  project, label, robot, policy, rule, webhook and the rest).
- GC facing calls and counters are idempotent: `BlobRepo::delete` and every `touch_*` and
  `mark_*_notified` call succeed on a missing row, `AssetRepo::delete_by_path` and
  `delete_by_path_checked` return `None` for a missing path, `BlobRepo::delete_if_unreferenced`
  returns `Ok(false)`, `record_downloads` ignores an asset that no longer exists. The one
  exception is `AssetRepo::touch_fresh`, whose own documentation says `NotFound`.
- A foreign key violation is `Conflict` (that is what `rustryx-db` maps SQLSTATE 23503 to):
  creating a member, a label, a robot, a hold or a policy for a project, repository or
  component that does not exist is `Conflict`. Table `CHECK` constraints are the `validate()`
  methods of the entities and are reported as `Conflict` too. Links to unknown rows that the
  port documentation does not mention are looser, see section 4.
- A write against a stale version is `Conflict` and moves the row to `version + 1`
  (`updated_at` comes from the store clock). The check comes after the `NotFound` check.
  Unique per scope policies (`RetentionPolicyRepo::upsert`, `upsert_scan_policy`,
  `upsert_trust_policy`, `NotificationRepo::upsert`, `PromotionRepo::set_target`) take
  `Some(version)` to replace the row of the scope and `None` to insert; `None` against an
  existing row and `Some` against none are `Conflict`.
- Fields an edit must not change are not taken from the caller: the format and type of a
  repository, the kind of a blob store, the scope of a rule, label, exception or webhook,
  the kind and schedule progress of a task, the stored secret of a webhook. This keeps an
  edit made from a stale read from undoing a concurrent change.
- Keyset pagination: the token is the hex of the last key of the page, so it survives
  inserts and deletes between requests. A malformed token is `StoreError::Invalid`. Limit is
  clamped to `1..=1000`. Keys: id for components, path for assets, username for users, name
  for projects, labels and robots, `(severity, vuln, package)` for findings, newest first for
  run histories and the audit log.
- Time comes from the store clock (`with_clock`), never from the caller, wherever the real
  schema uses `now()`: leases, expiries, `seen_at` of a node, `updated_at`.

### 3.2 Content and the SQL guards

`AssetRepo::publish_with`, `link`, `delete_by_path_checked`, `ComponentRepo::delete_checked`
and `LifecycleRepo::apply_deletions` run the guards that PostgreSQL runs as triggers, inside
one step under the table lock, so a refusal leaves nothing behind (no asset, no blob row, no
outbox row). The phase 1 methods (`publish`, `delete_by_path`, `ComponentRepo::delete`,
`RepositoryRepo::delete`, `find_or_create` of a new component) hit the same guards and report
a violation as `StoreError::Conflict`, which is what an N-1 writer sees.

Order of the refusals of `publish_with`: blob row conflict, read only mode, write policy,
`CreateOnly`, immutability, hold, quota. A test that provokes two at once learns this order;
nothing else promises it.

| Guard | What it does here |
|---|---|
| Write policy | Evaluated for hosted repositories only. `Deny` refuses, `AllowOnce` refuses a second write to an existing path. Proxy and group repositories are written by the system (cache fills), and the fixtures give proxy repositories `Deny`, so the setting is not applied to them. `rustryx-db` is expected to match; this is a reading of the port text, not a statement in it. |
| `PublishMode::CreateOnly` | An existing path is `PolicyViolation::AlreadyExists`. |
| Immutability (`RX004`) | Applies to an overwrite with different content and to a delete, never to the creation of a new path. A rule covers an asset by path (`ItemKind::Path`) or, for an asset with a component, by the component version (`ItemKind::Version`); deleting a component checks its version and every asset it takes along. A rule is found in the scope of the repository or of its project, by the regular expressions of `rustryx_core::glob`, the text PostgreSQL stores next to a glob. An overwrite with the same SHA-256 is an idempotent re-push and is allowed. `WriteOptions::override_immutable` lifts this guard only. `RepositoryRepo::delete` is guarded too, as the cascade is. A rule that cannot be evaluated covers the item (fail closed). |
| Holds (`RX005`) | An active `Legal` hold on the component, its repository or its project stops overwrite and delete. Release and promotion holds do not stop the guard, they stop lifecycle jobs through `LifecycleHoldRepo::active_for`. A released or expired hold does nothing. The override does not lift a legal hold. |
| Quota (`RX006`) | Checked on the actual size, repository scope then project scope. The delta counts a blob once per scope (`link` of a blob the scope already references adds nothing) and credits the replaced blob when the write took its last reference. `QuotaExceeded` carries scope, limit, used and requested bytes. |
| Read only (`RX007`) | While `system.read_only` has `enabled: true`: publish, link, deletes, creating a component row, the cascade of a repository delete and `apply_deletions` are refused. Finding a component, downloads, counters and `touch_*` are not. A statement level trigger fires for a cascade whether or not rows match, so deleting even an empty repository is refused; this is a reading of the schema that the conformance suite should confirm. |

More behaviour of the content ports:

- `publish_with` returns the replaced blob only when the write took its last reference
  (after `link` a blob can be shared). `delete_by_path` and `delete_by_path_checked` return a
  blob only when it lost its last reference.
- The blob row is unique by id and by `(store, key)`, as in the schema, so a second publish
  needs a fresh key. `fixtures::blob_record` takes the key from the caller; use a unique one.
- `link` needs the blob to exist (`NotFound`) and the target repository to be able to serve its
  store: the same store, or a group store that lists the blob's store as a member. Anything
  else is `Conflict` and the caller copies bytes.
- Garbage collection ages a blob from the moment its last asset went away (`orphaned_at`),
  or from `created_at` for a blob that was never referenced, exactly as the real queries
  `COALESCE(orphaned_at, created_at)` do. A relinked blob is referenced again.
  `delete_if_unreferenced` re-checks age and references under one lock and returns `Ok(false)`
  for a blob that gained a reference, is too young or does not exist. The double has no
  objects, so the "row first, object second" order has nothing to race with.
- `record_downloads` moves `last_downloaded_at` of the asset and its component forward (the
  double has no flush interval, so it always moves) and adds the count to the daily counter
  of the UTC date of the tick. `DownloadStatsRepo::daily` returns the last `days` days
  (at most 90), oldest first, with gaps filled by zero. `touch_fresh` moves `updated_at`
  forward only.
- Events of `WriteOptions::events` go to the outbox in the same step. An event over 16 KiB is
  `StoreError::Invalid` and nothing of the write happens. A write that crosses the warn
  percentage of a quota scope upward writes one `quota.near_threshold` event; the edge is
  re-armed when a delete takes usage below the warn percentage minus 5.

### 3.3 Projects, labels and robots

- `ProjectRepo::create` with `first_admin` inserts the member row in the same step.
  `update` replaces description and visibility only (the name is the URL), and rewrites the
  project copy and `allow_anonymous` of every attached repository, like the
  `projects_visibility_sync` trigger. `RepositoryRepo::create` and `update` apply the
  `repositories_project_sync` trigger: a repository in a project is public exactly when the
  project is, whatever the caller wrote; `update` never moves a repository between projects
  (`attach_repository` and `detach_repository` own the link). A `Repository::project` that
  names an unknown project is `Conflict` on create.
- `delete` is `Conflict` while a repository is attached. Members, robots, project labels and
  their links, the quota row, immutability rules, holds, retention, scan and trust policies,
  exceptions, webhooks and subscriptions of the project go with it.
- `attach_repository` is `Conflict` when the repository sits in another project, a no-op for
  the same one, and drops the label links of the repository's components that belong to
  labels of another project. `detach_repository` sets `allow_anonymous` to false.
- `ProjectMemberRepo`: one row per subject per project (a user, or a provider and an exact
  group string, never a prefix). Demoting or removing the last `project_admin` (direct or by
  group) is `Conflict` unless `allow_last_admin`. Events: `User(id)` for a user, `Projects`
  for a group row. `UserRepo::delete` removes the user's own rows, not group rows.
- `ProjectAccessRepo::group_mappings` answers `StoreError::Invalid` when more than `limit`
  rows exist, so a runaway table fails closed.
- `ProjectRepo::list_visible` takes the filter `RbacChecker::visible_projects` produced:
  `All`, `PublicOnly`, or `Member` (public projects plus those where the user id, or the
  provider and one of the exact groups, has a row). A filter this build does not know sees
  the public projects only. `query` is a case insensitive substring of the name.
- Labels: unique name per scope. A project label on a component of another project is
  `Conflict` (the `RX008` scope guard); more than 50 labels on one component is `Invalid`;
  `apply` is idempotent. A label of a missing component or label is `NotFound`. `delete`
  returns how many components lost it.
- Robots: unique by name and by hash. `rotate` replaces the hash at once and never re-enables
  a disabled robot. `list_expiring` returns usable robots whose expiry lies in the future and
  inside the window and that have no notice yet, soonest first. Derived state for the filter,
  most final first: revoked, disabled, expired, active. `TokenRepo::list_expiring` and
  `mark_expiry_notified` are overridden with the same rules.

### 3.4 Policies, runs and garbage collection history

- Immutability rules: validated, at most 15 per scope, the scope must exist. `preview` lists
  existing asset paths or component versions a rule would cover in its scope (OCI tags are not
  modelled, so a tag rule previews nothing).
- Quotas are computed from the assets on every read (distinct blobs per scope), so the counter
  of the real schema cannot drift here and `recount` only stamps `recounted_at` and returns
  `(used, used)`. A scope has a quota row when a limit was set or a recount ran. The project
  limit is versioned (a missing row stands at version 0, so the first write takes `Some(0)` or
  `None`); the repository limit is the column `repositories.quota_bytes`, which has no version,
  so `expected_version` is ignored for it. `headroom` is the smallest remaining room over the
  repository and its project.
- `LifecycleRepo::candidates` returns components of a project or repository in the order the
  engine needs (`repository_id, group_id, name, published_at DESC, id`), paged with a token.
  `apply_deletions` answers one result per input: `Deleted { bytes }`, `Skipped` for an
  immutable or held component (override lifts immutability, never a legal hold) and `Failed`
  for a component that is already gone. Read only mode aborts the whole batch with `Conflict`.
  The run must exist (`Conflict` otherwise) and receives one item per input.
- `LifecycleRunRepo::last_dry_run` only counts a dry run that finished `Succeeded`, of the
  same policy version, started less than `max_age` ago. At most 100 000 items per run; the
  call says when the list was cut. `GcRunRepo::last_manual_start` counts a queued manual run
  from its creation. A garbage collection sample is capped at 1000 rows per run.
- `RetentionPolicyRepo::upsert` keeps the id of the row of the scope on replace.

### 3.5 Supply chain

- Every write to a scan policy, an exception, a trust policy, trust material and a quarantine
  change bumps the `system_config` row `security.revision` and publishes
  `InvalidationEvent::Config`. That row is neither `settings.*` nor `system.*`, so it does not
  move `SystemConfigRepo::current_revision`.
- `ScanRepo::complete` stores the report as `Complete`, drops the findings of the previous
  current report, inserts the new ones (more than 50 000, a duplicate key or an invalid
  finding is refused), flips `is_current`, upserts the component state (vulnerability reports
  only) and keeps the newest three reports per `(component, kind)`. `fail` leaves the previous
  report and verdict, changes only the status the state shows (so a failed rescan reads as not
  scanned, never clean), and cuts an error to 500 characters. A scan ends once: a second
  `complete` or `fail` is `Conflict`. `start` always stores the report queued and not current.
  A report needs an existing component.
- `effective_summary` counts the findings of the current report that match none of `allowed`
  (case sensitive on normalized ids) and reports the rest as `suppressed`.
  `stale_components` returns components that were never scanned or were scanned before the
  cutoff, newest component first.
- Exceptions: unique per scope and vulnerability. `active_ids` returns the component's own
  plus the project's (mode `Project`) or the system's (mode `System`), without expired ones.
  `take_expired_unaudited` marks what it returns, and a new expiry date re-arms the mark.
  `copy_system_to_project` copies the active system entries the project does not already
  have.
- `SecurityPolicyRepo::effective_for` merges field by field, repository over project over
  system, and uses the built-in default when no system row exists. `effective_trust_for`
  returns the most specific trust policy as a whole. A trust policy may only name trust
  material that exists, and material a policy lists cannot be deleted.
- `SecurityHubRepo::search` needs a vulnerability id (`Invalid` otherwise). A `tag` filter
  matches nothing (OCI tags are not modelled); `digest` matches the component version or name.
- `ExportRepo::claim_download` is allowed once, before expiry, and only for the user who
  requested the export when one is recorded. `ScannerDataRepo::publish` makes the new version
  current and the previous ones not; `prunable` keeps the newest two. A scan token must be a
  64 digit lowercase hex digest, and an expired one is not found.
- `ScannerRepo`: at most one default among the scanners that can find vulnerabilities;
  `set_default` moves the flag from every scanner that shares a capability and overlaps in
  formats (an empty format list covers all). An edit keeps the stored `last_probe`.

### 3.6 Distribution

- `RegistryRepo::update` rewrites `remote_url`, basic credentials and extra CAs of every proxy
  repository bound to the registry in the same step (a bearer registry leaves the proxy without
  credentials: a proxy repository carries basic auth only) and publishes `Reset`. A registry
  that a proxy repository or a replication rule uses cannot be deleted.
- Replication: one queued or running execution per rule (`start_execution` returns `None`),
  a rule cannot be updated while one runs, tasks are claimed with a lease (a task whose lease
  ran out is claimed again), and `finish_task` ends a task with a terminal status or puts it
  back with a retry delay (anything else is `Invalid`). Execution counters are computed from
  the tasks. The first claim moves a queued execution to running. A stop request ends the
  pending tasks as stopped when the execution settles. `settle_executions` finishes
  executions with no unfinished task (`Succeeded`, `PartiallyFailed`, `Failed` or `Stopped`),
  writes the `replication.finished` event, and waits 60 seconds before it settles an execution
  that never received a task, because `start_execution` and `add_tasks` are two calls.
  `create_event_tasks` joins the rule's active execution (the unique index leaves no other
  choice) or opens an event execution; the one minute window of the real job is not modelled.
- Webhooks: at most 20 per scope, the signing secret changes only through `rotate_secret`,
  `matching` returns enabled and not suspended webhooks of the event's system, project and
  repository scope, `record_outcome` suspends after `suspend_after` failures in a row (0
  means never). Deliveries are created once per `(webhook, event)`, claimed with a lease,
  finished only by the owner, and an excerpt is stored without control characters.
- The batch methods that take `advance` (`WebhookDeliveryRepo::create_for_events`,
  `NotificationRepo::create_for_events`, `ReplicationRepo::create_event_tasks`,
  `JobLeaseRepo::enqueue_batch`) compare and set the outbox cursor first; a lost compare
  writes nothing and returns 0. Mail identical in address, kind and subject within 10 minutes
  is stored as `Suppressed`.
- Promotion: `decide` stores the decision as given (the caller computed `counted`), refuses a
  second decision of one approver and a request that is not pending or has expired, moves the
  request to `Approved` when counted approvals reach the requirement and queues the executor
  job (queue `promotion`, dedupe key `promotion:<id>`) in the same step. A rejection ends the
  request. `cancel` is for the requester only. `mark_running` is a compare and set on
  `Approved`. `finish` needs an approved or running request and a terminal status.

### 3.7 System ports

- Outbox: every state changing call is one "transaction" with its own `tx_id`; all of them are
  finished the moment the call returns, so the relay's snapshot horizon never hides a row.
  `read_batch` returns events after the consumer's cursor in `(tx_id, seq)` order,
  `advance` is a compare and set, `lag` is the age of the oldest unhandled event, and `purge`
  removes handled events older than the cutoff but never past the slowest known cursor (with
  no cursor at all it removes nothing, because a consumer that starts later would lose them).
  It returns the number of events, where PostgreSQL drops partitions.
- Job leases: `claim` serves the oldest available row and takes over a running row whose lease
  ran out. `heartbeat`, `complete`, `fail` and `release_for_retry` return `false` unless the
  caller still holds the row (running, same owner); a lease that ran out but was not taken over
  can still be renewed, the rule `LockGuard::renew` has. A queued or running row with the same
  `(queue, dedupe_key)` makes an insert a no-op.
- Settings: `put` sealed fields move out of the stored value; a `Mask` read removes them and
  lists their pointers under `_sealed`, a `Reveal` read puts them back, a sealed field absent
  from a write keeps its text and a JSON null clears it. Revisions come from one sequence, a
  delete takes a revision too, and `current_revision` is the highest revision of a `settings.*`
  or `system.*` row.
- Task runs: `enqueue` returns `None` while a queued or running run of the same
  `(kind, scope_key)` exists; `claim` takes the oldest queued run of the given kinds; `release`
  does not count as an attempt; `reap_expired` requeues a run of a resumable kind up to three
  claims and abandons the rest; the log keeps 5000 lines and then one truncation line; `finish`
  writes the `task.finished` event when the definition asks for that outcome.
- Task definitions: `update` replaces schedule, enabled and notify (and name and params for
  a non system task), `delete` is `Conflict` for a system task, `seed` adds only the missing
  ones, `advance` is a compare and set of `next_run_at`.
- `NodeRegistryRepo::heartbeat` stamps `seen_at` with the store clock; `list_live(max_age)`
  returns nodes seen within `max_age`.
- Blob stores: `update` keeps the kind and publishes `Reset`. A group holds plain stores,
  groups do not nest, a store belongs to one group, a member that still holds blobs cannot
  leave, and the last member of a group that repositories use cannot leave.
  `convert_to_group` repoints the repositories in one step. `BlobRepo::move_blob` is a compare
  and set on the current store, and `list_page_by_store` is implemented (the port default is a
  failing `Invalid`). The move ledger is keyed by `(blob, from store)`.
- `MemBlobStore`: writes hash in slices of at most 64 KiB, an object becomes visible only at
  `commit`, `abort` and a dropped writer discard it, reads yield 8 KiB chunks and follow HTTP
  range rules (`end` past the last byte is clamped, `start` at or past the end is
  `RangeNotSatisfiable`). `capabilities()` says `lists_staging`, `lists_keys` and `resumable`,
  and `can_presign` once `set_presign_enabled(true)` is called. `list_keys` pages in key order
  with an opaque cursor (a malformed one is `BlobError::InvalidKey`). A resumable upload stages
  chunks at the expected offset (another offset is `RangeNotSatisfiable` and stages nothing),
  `finalize` joins, hashes and commits, and an abandoned upload shows up in
  `list_stale_staging` under its target key.
- `MemLockBackend::force_expire(key)` drops a lock. The old guard's `renew()` then
  returns `false` and `is_lost()` is `true`. A guard also reports `is_lost()` as soon as
  its TTL ran out, without waiting for a `renew()` (stricter than the port requires).

## 4. Where it is looser than PostgreSQL

Tests would otherwise need ceremony for no benefit, so these are not enforced. The guards of
section 3.2 are not among them.

- No foreign key checks where the port documentation does not name one: an asset may name a
  repository, a component or a blob store that does not exist (its repository guards are then
  skipped, because no scope can be named), a role may name privileges or nested roles that do
  not exist (they are ignored when privileges are resolved), a token may name any user, a
  project member any user id, a repository any routing rule, cleanup policy or registry id, a
  replication rule any component of its filter.
- `Repository` consistency (`proxy` only for proxy repositories, and so on) is not
  validated. `fixtures::repository` builds consistent values.
- Usernames are matched exactly (case sensitive).
- Deleting a role removes it from user assignments, from other roles' nesting and drops
  mappings that point at it. Deleting a privilege detaches it from roles. These stand
  for `ON DELETE CASCADE`.
- Counters never drift: quota usage and replication execution counters are computed on read.
  `QuotaRepo::recount` therefore reports no correction.
- Time based windows that depend on job timing are simplified: the one minute grouping of event
  executions, the flush interval of `record_downloads`, the 30 day log retention.
- OCI tags, referrers and the OCI tag table are not modelled: `Candidate::tags` is empty, a
  tag immutability rule previews and covers nothing, a Security Hub tag filter matches nothing.
- Sealing is a map, not encryption: sealed settings are kept in the clear inside the double and
  only handed out through `SecretView::Reveal`. `Debug` of an entry never prints a value.
- `OutboxRepo::purge` counts events, `PostgreSQL` drops partitions.
- Strictness the real schema may not have: `ExportRepo::claim_download` checks that the caller
  is the requester, `ScanRepo::mark_running` only accepts a queued scan, `PromotionRepo::finish`
  needs an approved or running request. The conformance suite decides which side is right.

Stricter than a bare map, because PostgreSQL would refuse as well: `BlobStoreRepo::delete`
while a repository uses the store or the store is a member of a group,
`BlobRepo::delete` while an asset references the blob, `SelectorRepo::delete` while a
privilege references the selector, `ProjectRepo::delete` while a repository is attached,
`CleanupPolicyRepo::delete` while a repository lists the policy, `RoutingRuleRepo::delete`
while a repository uses the rule, `RegistryRepo::delete` and `TrustedCertificateRepo::delete`
while something depends on them, `ScannerRepo::delete` while a scan policy names the scanner
(all `Conflict`).

## 5. Fixtures

```rust
use rustryx_testkit::fixtures::*;

let repo = repository("maven-hosted", RepoFormat::Maven2, RepoType::Hosted);
let new_user = user("alice", UserSource::Oidc);       // issuer and subject filled in
let principal = principal_with_roles(&["nx-developer", "my-role"]);
let blob = blob_record("some-key", b"content");       // size and digests computed
let resource = resource("maven-hosted", RepoFormat::Maven2, "org/acme/a.jar");
let payments = project("payments");                   // private, created now
let upload = new_asset(repo.id, "a/b.bin", b"data");  // a blob key that is unique per call
let body = collect_stream(blob_store.get(&key, None).await?).await;
```

`repository` panics on an invalid name, `collect_stream` panics when the stream errors
(use `try_collect_stream` to inspect the error). Both are for tests only.
`checksums_of(&[u8])` returns the digests the blob store would compute.

`resource(name, format, path)` builds a fixture repository and calls
`ResourceRef::for_repository`. There is deliberately no helper that takes the anonymous flag
as a loose argument: forgetting it is the bug the type exists to prevent. A test that needs an
anonymous readable resource sets `allow_anonymous` on a fixture repository and calls
`ResourceRef::for_repository` itself.

`Principal::is_admin` is gone (gap 15). `principal_with_roles(&["nx-admin"])` still carries the
role id of the seeded `nx-admin`, so `RbacChecker::is_admin` finds it in a seeded store, and
`principal.has_role(ROLE_NX_ADMIN)` is the name based check for tests that only look at the
output of the authentication chain.

## 6. Testing failure paths

```rust
let store = MemStore::new();
store.set_unavailable(true);                 // every port call fails with Unavailable
store.set_read_only(true, "backup");         // writes fail with PolicyViolation::ReadOnly

let blobs = MemBlobStore::new();
blobs.set_unavailable(true);                 // begin/get/head/delete/health/list_keys fail
blobs.set_keep_abandoned_staging(true);      // a dropped writer leaves staging behind
blobs.set_presign_enabled(true);             // presign_get answers a fake signed URL

let locks = MemLockBackend::with_clock(Arc::new(clock.clone()));
let mut guard = locks.try_acquire("job", ttl).await?.unwrap();
locks.force_expire("job");
assert!(!guard.renew().await?);
```

By default a dropped `BlobWriter` is cleaned up at once, so a service that forgets
`abort()` does not leak staging in tests. Turn `set_keep_abandoned_staging` on to
exercise the garbage collection of staging (`list_stale_staging`, then `delete`).

Time dependent behaviour (leases, expiry, windows) needs an injected clock:
`MemStore::new().with_clock(Arc::new(clock.clone()))`, then `clock.advance(duration)`. The
store reads the clock at every call, so a lease runs out when the test says so.

## 7. What is not here

The migration notes of core revision 2 list further test helpers that are not part of this
revision of the crate: `FakeRegistry` (an in process distribution server for the replication
tests), `FakeReceiver` (webhook tests), a fake SMTP server, and the shared harnesses of the
design document (`ssrf_matrix`, `canary`, `gate_matrix`, `guard_matrix`, `quota_race`,
`tenant_matrix`, `two_version_cluster`, `route_matrix`). They belong to crates that do not exist
yet (`rustryx-egress`, `rustryx-replication`, `rustryx-notify`) and need their outbound
clients to be written. The conformance suite that runs one set of assertions against `MemStore`
and `PgStore` (design section 9) is likewise not part of this crate yet; the behaviour of
section 3 is what it will pin.
