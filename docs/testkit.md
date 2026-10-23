# rustryx-testkit

In-memory implementations of the `rustryx-core` ports, so that tests of `rbac`, `auth`,
the format crates and `api` run without PostgreSQL or S3. Add it as a dev dependency:

```toml
[dev-dependencies]
rustryx-testkit.workspace = true
```

Nothing here is for production: the crate is not a dependency of any binary.

## What is in it

| Item | Implements | Notes |
|---|---|---|
| `MemStore` | `RepositoryRepo`, `BlobStoreRepo`, `BlobRepo`, `ComponentRepo`, `AssetRepo`, `UserRepo`, `RoleRepo`, `PrivilegeRepo`, `SelectorRepo`, `TokenRepo`, `IdpMappingRepo`, `LoginAttemptRepo` | One value, all ports. Cheap `Clone`, clones share state. |
| `MemBlobStore` | `BlobStore` | Streaming writes, incremental SHA-1/SHA-256/MD5, ranges. |
| `FixedClock` | `Clock` | Moves only on `advance` and `set`. |
| `MemLockBackend` | `LockBackend` | TTL against tokio time or an injected clock. |
| `MemAuditSink` | `AuditSink` | `events()` returns what was recorded. |
| `MemInvalidationBus` | `InvalidationBus` | `tokio::sync::broadcast`; a lagging subscriber gets `Reset`. |
| `fixtures` | | Entities and helpers, see below. |

## Using `MemStore`

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
- `.with_clock(Arc<dyn Clock>)` makes `created_at` and similar stamps come from a
  `FixedClock`. Call it before cloning the store.
- `.with_invalidation_bus(bus)` publishes events after RBAC relevant writes:
  `InvalidationEvent::Rbac` for roles, privileges, selectors and mappings,
  `InvalidationEvent::User(id)` for role replacement, enable/disable, token validity,
  user deletion and token revocation.
- `set_unavailable(true)` makes every port call fail with `StoreError::Unavailable`
  (login guard fail-closed paths). Clones share the switch.
- `insert_blob(record)` seeds a blob row no asset references, for garbage collection
  tests. Published blobs arrive through `AssetRepo::publish`.

## Semantics

These follow the port documentation. Where the port is silent, this is what the double
does.

- Duplicates (name, id, token hash, external identity) are `Conflict`. A missing target of
  `update`, `delete`, `set_*`, `revoke` and `touch_*` is `NotFound`.
- `AssetRepo::publish` checks everything before writing, then inserts the blob row and
  creates or switches the asset in one step. It returns the replaced blob. The old blob
  row stays until `BlobRepo::delete`, so `list_unreferenced` reports it.
- `UserRepo::replace_roles_for_origin` replaces only rows of that origin. The role ids
  are validated first, an unknown one is `NotFound` and changes nothing.
- `RoleRepo::create` and `update` reject nesting cycles with `Conflict`.
  `effective_privileges` walks nesting with a visited set, so it terminates on cyclic
  data too.
- Keyset pagination: the token is the hex of the last key of the page, so it survives
  inserts and deletes between requests. The key is the id for components, the path for
  assets and the username for users. Limit is clamped to `1..=1000`.
- `MemBlobStore` writes hash in slices of at most 64 KiB, an object becomes visible only
  at `commit`, `abort` and a dropped writer discard it, reads yield 8 KiB chunks and
  follow HTTP range rules (`end` past the last byte is clamped, `start` at or past the
  end is `RangeNotSatisfiable`).
- `MemLockBackend::force_expire(key)` drops a lock. The old guard's `renew()` then
  returns `false` and `is_lost()` is `true`. A guard also reports `is_lost()` as soon as
  its TTL ran out, without waiting for a `renew()` (stricter than the port requires).

## Where it is looser than PostgreSQL

Tests would otherwise need ceremony for no benefit, so these are not enforced:

- No foreign key checks on insert: an asset may name a repository, a component or a blob
  store that does not exist, a role may name privileges or nested roles that do not
  exist (they are ignored when privileges are resolved), a token may name any user.
- `Repository` consistency (`proxy` only for proxy repositories, and so on) is not
  validated. `fixtures::repository` builds consistent values.
- `update` of a repository refuses to change `format` or `repo_type` (`Conflict`) and
  keeps `created_at`. `update` of a role or privilege never changes the `builtin` flag.
- Usernames are matched exactly (case sensitive).
- Deleting a role removes it from user assignments, from other roles' nesting and drops
  mappings that point at it. Deleting a privilege detaches it from roles. These stand
  for `ON DELETE CASCADE`.
- A malformed pagination token is reported as `NotFound("page token")`, because
  `StoreError` has no invalid-input variant.

Stricter than a bare map, because PostgreSQL would refuse as well: `BlobStoreRepo::delete`
while a repository uses the store, `BlobRepo::delete` while an asset references the blob,
`SelectorRepo::delete` while a privilege references the selector (all `Conflict`).

## Fixtures

```rust
use rustryx_testkit::fixtures::*;

let repo = repository("maven-hosted", RepoFormat::Maven2, RepoType::Hosted);
let new_user = user("alice", UserSource::Oidc);       // issuer and subject filled in
let principal = principal_with_roles(&["nx-developer", "my-role"]);
let blob = blob_record("some-key", b"content");       // size and digests computed
let body = collect_stream(blob_store.get(&key, None).await?).await;
```

`repository` panics on an invalid name, `collect_stream` panics when the stream errors
(use `try_collect_stream` to inspect the error). Both are for tests only.
`checksums_of(&[u8])` returns the digests the blob store would compute.

## Testing failure paths

```rust
let blobs = MemBlobStore::new();
blobs.set_unavailable(true);                 // begin/get/head/delete/health fail
blobs.set_keep_abandoned_staging(true);      // a dropped writer leaves staging behind

let locks = MemLockBackend::with_clock(Arc::new(clock.clone()));
let mut guard = locks.try_acquire("job", ttl).await?.unwrap();
locks.force_expire("job");
assert!(!guard.renew().await?);
```

By default a dropped `BlobWriter` is cleaned up at once, so a service that forgets
`abort()` does not leak staging in tests. Turn `set_keep_abandoned_staging` on to
exercise the garbage collection of staging (`list_stale_staging`, then `delete`).
