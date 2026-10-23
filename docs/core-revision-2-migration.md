# Core revision 2: migration notes for the crate owners

Status: `rustryx-core` carries Core revision 2 (docs/wave2-design.md section 1, gap list docs/core-gaps.md,
conflicts list docs/api-conventions.md section 19). The other crates do not compile against it on purpose:
`#[non_exhaustive]` types lose their struct literals, new required trait methods exist, and `Principal::is_admin`
is gone. This document tells each crate owner exactly what to change. The compiler stays the authority: the
lists below come from `cargo check --workspace --all-targets --locked --keep-going` and from a search of the
test targets, which cargo only reaches once the library of a crate compiles.

Contents:

1. [Where things stand](#1-where-things-stand): compile errors per crate, the order of work.
2. [Patterns](#2-patterns): the nine kinds of change, each with the old to new mapping and one example.
3. [Per crate](#3-per-crate): testkit, db, blob, auth, rbac, format-core, api, server, config, observability, and the crates that do not change.
4. [Migration 0007](#4-migration-0007-wave2-foundation): what `rustryx-db` writes.
5. [Things the design document left open](#5-things-the-design-document-left-open): choices made in core that a crate owner can rely on.

## 1. Where things stand

Compile errors seen when `rustryx-core` is the only crate that was adapted (unique `file:line` entries of the library and its test target; a crate whose library does not compile never reaches its integration tests, so those numbers grow after the first fix):

| Crate | Errors | Kinds |
|---|---|---|
| `rustryx-testkit` | 17 | 5 missing trait items (`E0046`), 11 struct literals (`E0639`), 1 removed method (`E0599`) |
| `rustryx-db` | 15 | 5 missing trait items, 6 struct literals, 4 non exhaustive matches (`E0004`) |
| `rustryx-format-core` | 14 | 7 struct literals, 7 non exhaustive matches |
| `rustryx-auth` | 8 | 7 struct literals, 1 non exhaustive match |
| `rustryx-api` | 8 | 2 struct literals, 6 non exhaustive matches |
| `rustryx-blob` | 7 | 5 struct literals, 1 missing trait item (test double), 1 non exhaustive match |
| `rustryx-rbac` | 3 | 3 non exhaustive matches |
| `rustryx-config`, `rustryx-observability`, `rustryx-tasks`, `rustryx-format-{maven,npm,pypi,oci,helm,raw,pub}` | 0 | they depend on core only for types that did not break |
| `rustryx-server` | not reached | depends on `rustryx-api`, which does not compile yet |

Order of work (design document 1.6): `rustryx-testkit`, `rustryx-db` (with migration 0007), `rustryx-blob`, `rustryx-rbac`, `rustryx-auth`, `rustryx-format-core`, `rustryx-api`, `rustryx-server`. Testkit comes first because auth, rbac and format-core use it in their tests. After each crate: `cargo fmt -p <crate>`, `cargo clippy -p <crate> --all-targets --locked -- -D warnings`, `cargo test -p <crate> --locked`.

Two workspace facts that changed:

* `rustryx-core` now enables the `time` feature `serde-well-known` in its own manifest (feature unification applies it to the whole workspace) and exports the serde helper `rustryx_core::rfc3339`. The lead may add `serde-well-known` to the `time` line of the root `Cargo.toml` so that the feature is stated where the other workspace features are.
* `rustryx-core` has two new dev dependencies (`proptest`, `regex`, both already in the workspace). `Cargo.lock` gained exactly those two edges.

## 2. Patterns

### P1. Struct literals become constructors

A `#[non_exhaustive]` struct cannot be built with a struct expression outside its crate. Build it with the constructor and assign the optional fields; all fields stay `pub`, so reading and assigning them is unchanged.

| Type | Constructor | What it sets | Assign afterwards when needed |
|---|---|---|---|
| `Repository` | `Repository::new(id, name, format, repo_type, blob_store_id, now)` | `WritePolicy::Allow`, `allow_anonymous` false, `online` true, no quota, no policies, no routing rule, no proxy, no group, empty `format_config`, no project, both times `now` | every other field |
| `ProxyConfig` | `ProxyConfig::new(remote_url)` | no auth, content 3600 s, metadata 300 s, negative 60 s, 8 concurrent, no registry, no extra CA, empty namespace allowlist, no bandwidth limit | `auth`, the four numbers, the four new fields |
| `BlobStoreDef` | `BlobStoreDef::new(id, name, kind, config, now)` | no quotas, no group | `quota_bytes`, `soft_quota`, `group` |
| `Component` | `Component::new(id, repository_id, format, name, now)` | no namespace, version, uploader, download time | `group_id`, `version`, `created_by`, `last_downloaded_at`, `updated_at` |
| `NewComponent` | `NewComponent::new(repository_id, format, name)` | no namespace, version, uploader | `group_id`, `version`, `created_by` |
| `Asset` | `Asset::new(id, repository_id, path, blob_id, content_type, size_bytes, checksums, now)` or `Asset::from_blob(id, repository_id, path, content_type, &blob, now)` | no component, download time or uploader, both times `now` | `component_id`, `last_downloaded_at`, `created_by`, `updated_at` |
| `NewAsset` | `NewAsset::new(repository_id, path, content_type, blob)` | no component, no uploader | `component_id`, `created_by` |
| `PublishOutcome` | `PublishOutcome::new(asset, replaced, created)` | | |
| `NewAssetLink` | `NewAssetLink::new(repository_id, path, content_type, blob_id)` | | `component_id`, `created_by` |
| `Principal` | `Principal::new(username, source, auth_method)` | everything else empty or `None` | `user_id`, `issuer`, `subject`, `roles`, `external_groups`, `groups_confirmed_at`, `token_scopes`, `token_id`, `email`, `display_name`, `external_id`, `expires_at` |
| `ResourceRef` | `ResourceRef::for_repository(&repository, path)`, then `.with_coordinate(c)` | copies name, format, `allow_anonymous` and project from the repository | |
| `Coordinate` | `Coordinate::new(group, name, version)` (all `Option<String>`) | extension and classifier `None` | `extension`, `classifier` |
| `ExternalGroups` | `ExternalGroups::new(provider, groups, confirmed_at)` | | |
| `AuthContext` | `AuthContext::new()`, `.with_client_ip(ip)`, `.with_request_id(id)`, `.with_deadline(instant)` | | |
| `AuditEvent` | `AuditEvent::new(kind, actor_name, action, outcome)` | `at` is now, actor kind `User`, no id, address, request id, target, project; detail `{}` | every other field (override `at` when a `Clock` is injected) |
| `BlobMeta` | `BlobMeta::new(size_bytes)` | `modified` `None` | `modified` |
| `BlobCommit` | `BlobCommit::new(size_bytes, checksums)` | `placement` `None` | `placement` |

Example (`rustryx-auth/src/basic.rs`, same shape in `tokens.rs`, `internal_jwt/mod.rs`, `ldap/authenticator.rs`, `oidc/bearer.rs`):

```rust
// before
Ok(Some(Principal {
    user_id: Some(user.id),
    username: user.username,
    source: UserSource::Local,
    issuer: None, subject: None,
    roles: Vec::new(), external_groups: Vec::new(),
    groups_confirmed_at: None, token_scopes: None, token_id: None,
    auth_method: AuthMethod::BasicLocal,
}))

// after
let mut principal = Principal::new(user.username, UserSource::Local, AuthMethod::BasicLocal);
principal.user_id = Some(user.id);
Ok(Some(principal))
```

Example (`rustryx-db/src/repos/rows.rs`, a row mapping builds the whole entity from columns):

```rust
let mut asset = Asset::new(
    AssetId::from_uuid(self.id), RepositoryId::from_uuid(self.repository_id), self.path,
    BlobId::from_uuid(self.blob_id), self.content_type, to_u64(self.size_bytes, "read asset")?,
    Checksums { sha1: self.sha1, sha256: self.sha256, md5: self.md5 }, self.created_at,
);
asset.component_id = self.component_id.map(ComponentId::from_uuid);
asset.last_downloaded_at = self.last_downloaded_at;
asset.created_by = self.created_by.map(UserId::from_uuid);   // new column, migration 0007
asset.updated_at = self.updated_at;
Ok(asset)
```

Tests that built a `ResourceRef` from three strings now need a repository. `rustryx-testkit` should offer `fixtures::resource(name, format, path)` that builds a fixture `Repository` and calls `ResourceRef::for_repository`; there is deliberately no public constructor that takes the anonymous flag as a loose argument, because forgetting it is the bug the type exists to prevent.

### P2. Exhaustive matches on enums that can grow

These enums are `#[non_exhaustive]` now, so a `match` outside core needs a wildcard arm: `StoreError`, `BlobError`, `AuthError`, `DomainError`, `UpstreamError`, `PolicyViolation`, `MutationError`, `AuthMethod`, `PrivilegeGrant`, `AuditKind`, `ActorKind`, `InvalidationEvent`, `BlobStoreKind`, `RepoFormat`, `EventKind`, `TaskRunStatus`, `TaskTrigger`, `ScanStatus`, `ExecutionStatus`, `PromotionStatus`, `FeatureId`, `ProjectPermission`, `AppArea`, `SettingsSection`, `ProjectVisibilityFilter`. The rule for the wildcard arm: an unknown value is never an allow and never a 404. Pick the arm that fails closed for that match.

| Match site | Wildcard arm |
|---|---|
| HTTP status of an error | 500 class (`internal_error`), the cause goes to the log |
| authorization on a `PrivilegeGrant` | the grant does not apply (`false`), never `true` |
| cache invalidation on an `InvalidationEvent` | drop everything (same as `Reset`) |
| text mapping to a database column | `AuditKind`, `ActorKind`: use `as_str()`; `BlobStoreKind`, `PrivilegeGrant`: return a typed error, never a default value |
| metric label of an `AuthMethod` | `"unknown"` |

Example (`rustryx-rbac/src/rbac.rs:277`, `:359`):

```rust
let allowed = match &privilege.grant {
    PrivilegeGrant::Admin => !subject.anonymous,
    PrivilegeGrant::RepositoryView { format, repository } => { /* unchanged */ }
    PrivilegeGrant::RepositoryContentSelector { repository, selector_id } => { /* unchanged */ }
    // `Application` grants administration areas, never content (design 3.e rule 5), and a grant
    // this build does not know cannot be evaluated: both allow nothing here.
    _ => false,
};
```

`RepoFormat::ALL` is a `&'static [RepoFormat]` now (was `[RepoFormat; 8]`). `for f in RepoFormat::ALL` yields `&RepoFormat`: write `for &f in RepoFormat::ALL` or `.iter().copied()`. `RepoFormat::ALL.to_vec()` still compiles.

### P3. New required trait methods (implementers: testkit and db, blob test doubles)

| Trait | New required methods | Contract in one line |
|---|---|---|
| `BlobStoreRepo` | `update(&BlobStoreDef)` | replaces config, quotas and group settings; `NotFound` when absent; tells the registry cache through the bus |
| `BlobRepo` | `insert(&BlobRecord)`, `list_unreferenced_in(store, older_than, limit)`, `delete_if_unreferenced(id, older_than) -> bool` | `insert` is `Conflict` on duplicate id or `(store, key)`; the sweep step locks the row `FOR UPDATE`, re-checks age and references, deletes the ROW and commits; `Ok(false)` keeps the blob; the caller deletes the OBJECT only after `Ok(true)` (row first, object second) |
| `ComponentRepo` | `delete_checked(id, &WriteOptions) -> Result<(), MutationError>` | same guards as `delete`, typed violation, events of the options in the same transaction |
| `AssetRepo` | `publish_with`, `link`, `delete_by_path_checked`, `touch_fresh`, `record_downloads` | `publish_with` is the enforcing publish (write policy, immutability, hold, quota on the actual size, read only mode, outbox events, all in the transaction that switches the asset); `publish` is now PROVIDED and calls it |
| `UserRepo` | `set_password_hash_if(id, old_hash, new_hash) -> bool` | compare and set; `false` when the stored hash is no longer `old_hash` |

Provided with a safe default, override them where the stream needs them: `BlobRepo::move_blob` (default `Ok(false)`), `BlobRepo::list_page_by_store` (default `StoreError::Invalid`, never an empty page), `TokenRepo::list_expiring` (empty) and `TokenRepo::mark_expiry_notified` (no op), `BlobStore::{capabilities, shutdown, list_keys, presign_get, resumable}`, `Authenticator::authenticate_detailed`, `RbacChecker::{is_admin, check_project, check_app, holds_whole_repository, visible_projects}`.

The old state changing methods stay callable and stay correct: the SQL guards (`RX004` to `RX008`, migration 0042 and later) apply to them too, and a violation reaches an old caller as `StoreError::Conflict` (`MutationError::into_store_error`). Phase 1 code therefore compiles and behaves; only new code calls the typed variants.

Example (`rustryx-testkit/src/store/users.rs`, the shape only: adapt it to the lock type of the store):

```rust
async fn set_password_hash_if(&self, id: UserId, old_hash: &str, new_hash: &str)
    -> Result<bool, StoreError>
{
    let mut state = self.state.write();
    let user = state.users.get_mut(&id).ok_or_else(|| StoreError::NotFound("user".into()))?;
    let current = user.password_hash.as_ref().map(Secret::expose);
    if current != Some(old_hash) {
        return Ok(false);
    }
    user.password_hash = Some(Secret::new(new_hash));
    Ok(true)
}
```

### P4. `Principal::is_admin()` is removed

Reason (gap 15): the method matched by role NAME and the roles of a principal are references that the chain fills in. A reference that only claims `nx-admin` passed it.

| Where | Old | New |
|---|---|---|
| production code | `principal.is_admin()` | `rbac.is_admin(&principal).await?` (`RbacChecker::is_admin`, matches the role id against the store and requires an unrestricted token) |
| tests that only want to look at the chain output | `assert!(p.is_admin())` | `assert!(p.has_role(ROLE_NX_ADMIN))` (name based, documented as display and tests only) |
| tests that prove a forged name does not grant admin | `assert!(!forged.is_admin())` | `assert!(!rbac.is_admin(&forged).await.unwrap())` |

Sites found: `rustryx-testkit/src/fixtures.rs:253`, `rustryx-rbac/tests/rbac_must_not.rs:536` (the forged name case, use the third row), `rustryx-auth/tests/oidc_keycloak.rs:398`, `rustryx-auth/src/oidc/tests/claims.rs:63`, `rustryx-auth/src/chain/tests_roles.rs:83`, `:244`, `:274`.

### P5. Timestamps serialize as RFC 3339

Every timestamp field of a core entity that derives `Serialize` now carries `#[serde(with = "rustryx_core::rfc3339")]` (or `#[serde(default, with = "rustryx_core::rfc3339::option")]`). Before, with only the `serde` feature of `time`, `OffsetDateTime` serialized as the crate's nine number tuple, which no JSON client reads. Consequences:

* A DTO or a JSON column that embeds a core entity now holds `"2026-10-24T10:00:00Z"`. No crate stores a timestamped core entity as JSON today (searched: the only `serde_json` uses on entities are db's private `StoredProxy` and `GroupConfig`), so no stored data changes.
* A new DTO that has its own `OffsetDateTime` uses the same attribute, `#[serde(with = "rustryx_core::rfc3339")]`, and needs no `time` feature of its own.
* A core test (`must_not_serialize_a_timestamp_without_the_rfc3339_helper`) scans the sources of the entity modules and fails when a serializable type holds an `OffsetDateTime` without the helper.
* `LabelScope`, `PolicyScope`, `QuotaScope`, `NotifyScope`, `ReplicationScope` and `AllowlistScope` are adjacently tagged enums. As a struct field they nest: `{"scope": {"scope": "system"}}`. API DTOs flatten or rename them; the entities are for storage and tests.

### P6. Errors: new variants and where they map

| New | Meaning | HTTP |
|---|---|---|
| `StoreError::Invalid(String)` | malformed input: bad page token, number out of range. `rustryx-db` and the testkit stop returning `Conflict` for these (db.md section 5, "invalid page token") | 400 (`invalid_page_token` for a token) |
| `BlobError::InvalidKey(&'static str)` | caller bug, the key could escape the root. Replaces `Backend` with a `KeyError` source | 500 class, never 404 |
| `BlobError::InsufficientStorage` | disk full or below the free space floor | 507 |
| `BlobError::Overloaded { retry_after }` | no upload slot. Replaces `Unavailable` for slot exhaustion | 503 with `Retry-After` |
| `BlobError::Unsupported(&'static str)` | the store cannot do this (list keys, presign, resumable) | 500 class unless the caller probed `capabilities()` |
| `AuthError::AccessDenied(&'static str)` | no mapped group and no default role, or not provisioned. Replaces `GroupLookupFailed` and `AccountDisabled` in those two places | 403 |
| `UpstreamError` and `DomainError::Upstream` | one typed 502 and 504 mapping: `UpstreamError::http_status()` | 502, 504 for `Timeout` |
| `PolicyViolation`, `MutationError`, `DomainError::Violation` | policy refusals. `PolicyViolation::http_status()`: 507 for a quota, 503 for read only mode, 409 for the rest. `PolicyViolation::label()` is a bounded label for metrics and logs, NOT the API code | see column |

The API codes come from docs/api-conventions.md section 5.2. One conflict between the documents: the design says read only mode answers 503 `read_only`, the contract says `read_only_mode`. The contract wins for the wire; `rustryx-api` maps `PolicyViolation::ReadOnly` to `read_only_mode`.

Messages of `DomainError::{Invalid, Conflict, AlreadyExists}` reach clients verbatim (documented on the enum). `rustryx-api` carries the lint test over every service message (gap 31).

### P7. `InvalidationEvent` has two more variants

`Projects` (membership, group mapping, label scope or visibility changed) and `Config(i64)` (a settings section or a security policy changed, with the `system_config` revision). A node that does not know a payload decodes it as `Reset` (db.md section 9), which keeps an N-1 node correct. `rustryx-db`'s `encode_event` and `decode_event` learn `projects` and `config:<revision>`; every consumer match gets the arm from P2.

### P8. New helper methods that crate owners can use

Not required by the design document, added because the same arithmetic would otherwise be written in five crates (each is tested in core): `PolicyViolation::{label, http_status}`, `MutationError::into_store_error`, `AuditKind::{ALL, as_str, parse}`, `ActorKind::{as_str, parse}`, `AppArea::{ALL, as_str}`, `EventKind::{ALL, as_str, parse, webhook_subscribable, high_volume}`, `ProjectRole::{ALL, as_str, parse, template}`, `ProjectGrantSet::{allows_content, allows_project}`, `Severity::{ALL, index, as_str, from_scanner_label, gate_equivalent, blocks_at}`, `ScanSummary::{count, blocking}`, `normalize_vuln_id`, `RobotToken::{is_usable_at, full_name, validate_name, validate_short_name}`, `LifecycleHold::is_active_at`, `CveAllowlistEntry::is_active_at`, `Webhook::{is_deliverable, subscribes_to}`, `PromotionDecision::counts`, `PromotionRequest::is_expired_at`, `GroupMappingPolicy::project_admin_may_map`, `LoggingSettings::active_at`, `TrustedCertificate::is_valid_at`, `NameSelector::{to_regexes, selects}` and `glob::{validate, to_regex}`, `gate_open` and `nodes_below_level`, `BlobMove::new` (enforces the cleanup delay rule), `DownloadSource::counts_as_download`, `StatusCheck::unknown`.

### P9. Validation lives in core, enforcement in the service

Value types validate on `parse` and on `Deserialize` (`ProjectName`, `LabelName`, `RepoName`); entities carry a `validate()` that states the limits of the table constraints (`Project`, `Label`, `RobotToken` names, `CleanupPolicy`, `RetentionPolicy`, `ImmutabilityRule`, `LifecycleHold`, `ScannerRecord`, `Finding`, `CveAllowlistEntry`, `TrustPolicy`, `TrustMaterial`, `Registry`, `ReplicationRule`, `Webhook`, `NotificationSubscription`, `PromotionRequest`, `PromotionTarget`, `RoutingRule`, `TrustedCertificate`, and every settings section). A service calls `validate()` before it writes; a repository does not re-validate rows it reads. `Deserialize` of an entity does not validate: only the name newtypes do.

## 3. Per crate

### 3.1 rustryx-testkit (17 errors)

Breaking changes:

| File:line | What | Change |
|---|---|---|
| `src/store/catalog.rs:102` | `BlobStoreRepo` misses `update` | implement (P3); `NotFound` when absent |
| `src/store/catalog.rs:148` | `BlobRepo` misses `insert`, `list_unreferenced_in`, `delete_if_unreferenced` | implement (P3). `delete_if_unreferenced`: under one lock check age and references, remove the row, return `true`; the in memory store has no object to delete |
| `src/store/content.rs:56` | `ComponentRepo` misses `delete_checked` | implement with the same guards as `delete` |
| `src/store/content.rs:126` | `AssetRepo` misses `publish_with`, `link`, `delete_by_path_checked`, `touch_fresh`, `record_downloads` | implement; keep the existing `publish` body as `publish_with` (the trait now provides `publish`); remove the old `publish` override or keep it only if a test needs it |
| `src/store/users.rs:29` | `UserRepo` misses `set_password_hash_if` | implement (example in P3) |
| `src/store/content.rs:65`, `:158`, `:176` | `Component`, `Asset`, `PublishOutcome` literals | `Component::new`, `Asset::new`, `PublishOutcome::new(asset, replaced, created)` with `created = replaced.is_none()` |
| `src/blob_store.rs:133`, `:349` | `BlobCommit` literals | `BlobCommit::new` |
| `src/blob_store.rs:265` | `BlobMeta` literal | `BlobMeta::new` |
| `src/audit.rs:53` | `AuditEvent` literal | `AuditEvent::new`, then assign |
| `src/fixtures.rs:48` | `ProxyConfig` literal | `ProxyConfig::new(url)` then assign the four numbers |
| `src/fixtures.rs:57` | `Repository` literal | `Repository::new(...)` then assign `write_policy`, `proxy`, `group` |
| `src/fixtures.rs:84` | `BlobStoreDef` literal | `BlobStoreDef::new` |
| `src/fixtures.rs:126` | `Principal` literal | `Principal::new` then assign |
| `src/fixtures.rs:253` | `Principal::is_admin` | P4 |
| `tests/store_content.rs`, `tests/blob_store.rs`, `tests/contract.rs` | `NewComponent`, `NewAsset`, `BlobCommit`, `AuditEvent` literals | constructors (found by search, the compiler reports them after the library compiles) |

Also in this crate:

* The page token of every list: a malformed token is `StoreError::Invalid` now (gap 1), not `NotFound`.
* The documented semantics of testkit.md section "Semantics" get the rules of `ports/repos.rs` (absent targets: `NotFound` for admin facing entities, idempotent for GC facing calls; role and privilege deletes cascade to their link rows; a group mapping that names a role is `Conflict`). Add one conformance test per rule; the design (section 9) wants one suite that runs against `MemStore` and `PgStore`.
* `MemBlobStore` implements `capabilities()` (`lists_keys` true) and `list_keys`, and `shutdown`.
* Add `fixtures::resource(repository_name, format, path)` (P1).

New ports to implement in wave 2 (not now): in memory versions of every new port with the same guards as PostgreSQL (scope check, last project admin, immutability by the same glob translation, holds, quota, read only), namely `ProjectRepo`, `ProjectMemberRepo`, `ProjectAccessRepo`, `LabelRepo`, `RobotTokenRepo` (B); `QuotaRepo`, `ImmutabilityRepo`, `LifecycleHoldRepo`, `CleanupPolicyRepo`, `RetentionPolicyRepo`, `LifecycleRepo`, `LifecycleRunRepo`, `GcRunRepo`, `DownloadStatsRepo`, `OutboxRepo` (`MemOutbox`), `TaskDefinitionRepo`, `TaskRunRepo` (`MemTaskStore`), `NodeRegistryRepo` (C); `ScannerRepo`, `ScanRepo`, `CveAllowlistRepo`, `SecurityPolicyRepo`, `TrustRepo`, `SecurityHubRepo`, `ExportRepo`, `ScannerDataRepo` (D); `RegistryRepo`, `ReplicationRepo`, `WebhookRepo`, `WebhookDeliveryRepo`, `NotificationRepo`, `PromotionRepo`, plus `FakeRegistry` and `FakeReceiver` (E); `BlobStoreMemberRepo`, `BlobMoveRepo`, `VerifyRunRepo`, `BlobStoreStatsRepo`, a `MemGroupStore` (F); `SystemConfigRepo` (`MemSystemConfig`, with the sealing contract), `TrustedCertificateRepo`, a fake SMTP server (H); and from this revision, `RevokedTokenRepo`, `JobLeaseRepo`, `AuditReadRepo`, `RoutingRuleRepo`. The shared harnesses of design section 9 (`ssrf_matrix`, `canary`, `gate_matrix`, `guard_matrix`, `quota_race`, `tenant_matrix`, `two_version_cluster`, `route_matrix`) live here.

### 3.2 rustryx-db (15 errors)

Breaking changes:

| File:line | What | Change |
|---|---|---|
| `src/repos/asset.rs:118` | `AssetRepo` misses five methods | implement `publish_with` (the current transaction plus the guards and the quota statement of design 4.c), `link`, `delete_by_path_checked`, `touch_fresh`, `record_downloads`; the phase 1 `publish` becomes the provided wrapper, delete the override |
| `src/repos/asset.rs:178`, `rows.rs:79` | `PublishOutcome`, `Asset` literals | `PublishOutcome::new(asset, replaced, created)` (`created` is whether the asset row was inserted), `Asset::new` (example in P1) |
| `src/repos/blob.rs:30` | `BlobRepo` misses `insert`, `list_unreferenced_in`, `delete_if_unreferenced` | implement; `delete_if_unreferenced` is the row lock transaction of P3. Also implement `move_blob` and `list_page_by_store` when F lands (they are provided now) |
| `src/repos/blob_store.rs:79` | `BlobStoreRepo` misses `update` | implement; publish the bus event |
| `src/repos/blob_store.rs:51` | `kind_label` match on `BlobStoreKind` | add `Azure => "azure"`, `Group => "group"`; the database CHECK stays `local, s3` until migration 0100 and a store of the new kinds is created only after the `BlobGroupsAndAzure` gate is open, so reading an unknown text keeps returning a typed error |
| `src/repos/blob_store.rs:64` | `BlobStoreDef` literal | `BlobStoreDef::new` then assign `quota_bytes` |
| `src/repos/component.rs:101` | `ComponentRepo` misses `delete_checked` | implement |
| `src/repos/component.rs:50` | `Component` literal | `Component::new` then assign, plus the new columns `created_by`, `last_downloaded_at` |
| `src/repos/user.rs:143` | `UserRepo` misses `set_password_hash_if` | `UPDATE users SET password_hash = $3 WHERE id = $1 AND password_hash = $2`, `true` when one row changed, `NotFound` when the user does not exist |
| `src/repos/repository.rs:152`, `:212` | `ProxyConfig`, `Repository` literals | constructors then assign; read the new `project_id`, join `projects` for `Repository.project` (`ProjectRef`), keep `StoredProxy` in step with the four new `ProxyConfig` fields (`registry_id` resolves through the registry in wave 2) |
| `src/audit.rs:28` | `kind_label` match on `AuditKind` | replace by `kind.as_str()` (it is exhaustive inside core) |
| `src/invalidation.rs:29` | `encode_event` match | `Projects => "projects"`, `Config(rev) => format!("config:{rev}")`, wildcard arm `_ => "reset"`; `decode_event` already maps unknown payloads to `Reset`, add the two known ones |
| `src/repos/privilege.rs:69` | `grant_columns` match on `PrivilegeGrant` | add `Application { area }` with kind `"application"` and the area text in the `repository` column (or a new column, db owner decides) once the `ApplicationPrivileges` gate is open; wildcard arm returns a `StoreError::Invalid`, never a default grant |

Also in this crate:

* `error::map_err`: SQLSTATEs `RX004` immutable, `RX005` held, `RX006` quota exceeded, `RX007` read only, `RX008` scope violation map to `PolicyViolation` for the typed methods (`MutationError::Violation`) and to `StoreError::Conflict` for the old methods (db.md section 5 table gets five rows). Unique text per constraint stays; never echo the offending value.
* Page tokens and out of range numbers: `StoreError::Invalid`, not `Conflict` (db.md section 5, the paragraph "Pagination is keyset" currently says the opposite). Update the paragraph.
* `audit_events` rows carry `actor_kind` and `project_id`: `PgAuditSink` writes them, the text mapping accepts the seven new `AuditKind` values (the column is plain text).
* `rustryx-core` timestamps are RFC 3339 now; db's own row structs are unaffected.
* GC wording: `BlobRepo::delete` is documented as unsafe for garbage collection (row only, idempotent); the GC job in `rustryx-tasks` uses `delete_if_unreferenced` and deletes the object after it.

New ports to implement (wave 2): this revision's `RevokedTokenRepo` (table `revoked_tokens`), `JobLeaseRepo` (the inherent `PgJobLeases` API becomes the trait impl, add `enqueue_batch`, `depth`, `purge_finished`), `AuditReadRepo` (`query` keyset by `(event_time, id)`, `export` streaming), `RoutingRuleRepo` (migration of W2-A), `SystemConfigRepo` (`PgSystemConfig` becomes the impl, sealing by JSON pointer, revision from `system_config_revision_seq`), `OutboxRepo`, `TaskDefinitionRepo`, `TaskRunRepo`, `NodeRegistryRepo`; stream B: `ProjectRepo`, `ProjectMemberRepo`, `ProjectAccessRepo`, `LabelRepo`, `RobotTokenRepo`, the `TokenRepo` expiry methods; stream C: `QuotaRepo`, `ImmutabilityRepo`, `LifecycleHoldRepo`, `CleanupPolicyRepo`, `RetentionPolicyRepo`, `LifecycleRepo`, `LifecycleRunRepo`, `GcRunRepo`, `DownloadStatsRepo`; stream D: `ScannerRepo`, `ScanRepo`, `CveAllowlistRepo`, `SecurityPolicyRepo`, `TrustRepo`, `SecurityHubRepo`, `ExportRepo`, `ScannerDataRepo`; stream E: `RegistryRepo`, `ReplicationRepo`, `WebhookRepo`, `WebhookDeliveryRepo`, `NotificationRepo`, `PromotionRepo`; stream F: `BlobStoreMemberRepo`, `BlobMoveRepo`, `VerifyRunRepo`, `BlobStoreStatsRepo`; stream H: `TrustedCertificateRepo`. Migration ranges are in design section 2.7.

### 3.3 rustryx-blob (7 errors)

| File:line | What | Change |
|---|---|---|
| `src/hashing.rs:75` | `BlobCommit` literal | `BlobCommit::new(size, checksums)` |
| `src/local.rs:253`, `src/s3/mod.rs:304` | `BlobMeta` literals | `BlobMeta::new(size)`; set `modified` from the file metadata and from the S3 `last_modified` when available (the staging sweep and `verify_blobs` use it) |
| `src/registry.rs:73` | `build_parts` match on `BlobStoreKind` | add `Azure` and `Group` arms returning a `BuildError` ("kind not supported by this build") until stream F; a group is built by the F1 group adapter |
| `src/registry/tests.rs:27` | `FakeRepo` test double misses `BlobStoreRepo::update` | add it (a no op that returns `Ok`) |
| `src/registry/tests.rs:61`, `:153` | `BlobStoreDef` literals | `BlobStoreDef::new` |
| `tests/s3_minio.rs:386` | `OneStoreRepo` implements `BlobStoreRepo` | add `update` (found by search) |
| `src/s3/tests/support.rs` | `BlobCommit` literal | constructor (found by search) |

Also: map the gap 5 to 7 cases to the new variants (`InvalidKey` instead of `Backend` with a `KeyError` source, `InsufficientStorage` for a full disk, `Overloaded { retry_after }` for upload slot exhaustion, which used `Unavailable`); implement `BlobStore::shutdown` (replaces the side trait `CleanupDrain`); `BlobStore::capabilities` (`lists_staging` is false for S3 through `object_store`, so an empty `list_stale_staging` result there means "cannot tell").

New items to implement (wave 2, stream F): `list_keys` for local (shard walk), S3 and Azure; `presign_get` (S3, Azure after spike S5); `resumable()` for OCI chunked uploads (`ResumableBlobStore`, each chunk a staged object, `finalize` re-reads the chunks to hash); `GroupBlobStore` placement (`BlobCommit::placement`), `AzureBlobStore`; the `BlobStoreRegistry` adapter gains `store_by_id` and `store_for_blob` (the trait lives in format-core); the local disk free probe behind a `DiskProbe` trait for `InsufficientStorage`; the metrics `blob_store_ops_total`, `blob_store_bytes_total`. Consumes `BlobStoreRepo::update` and `BlobStoreMemberRepo`.

### 3.4 rustryx-auth (8 errors)

| File:line | What | Change |
|---|---|---|
| `src/basic.rs:130`, `src/tokens.rs:325`, `src/internal_jwt/mod.rs:505`, `src/ldap/authenticator.rs:171`, `src/oidc/bearer.rs:65` | `Principal` literals | P1. Fill the new fields where the credential carries them: `expires_at` from the JWT `exp` or the token `expires_at`; `email` and `display_name` from the OIDC claims; `external_id` from the LDAP user DN |
| `src/ldap/resolver.rs:271`, `src/oidc/bearer.rs:129` | `ExternalGroups` literals | `ExternalGroups::new(provider, groups, confirmed_at)`. The LDAP resolver sets `confirmed_at` to the time the directory answered (gap 23), the chain times its five minute window from it |
| `src/chain/mod.rs:202` | `invalidation_task` match on `InvalidationEvent` | `Some(InvalidationEvent::Projects \| InvalidationEvent::Config(_)) => {}` (neither changes a principal), `Some(_) => service.invalidate_all()` for a variant added later |
| (search) `tests/**`, `src/**/tests*.rs`, `src/chain/testing.rs`, `src/groups/jit/tests.rs`, `src/chain/cache.rs` | `Principal`, `ExternalGroups`, `AuthContext` literals | constructors |
| (P4) | `is_admin()` in 4 test files | `has_role(ROLE_NX_ADMIN)` |

Also: `AuthService::authenticate_detailed` override returns `AuthFailure { method, error }` so `auth_attempts_total` gets a real method (gap 32, today `unknown` on failures); report `AuthError::AccessDenied` where "no mapped group and no default role" was `GroupLookupFailed` and "not provisioned" was `AccountDisabled` (gap 20); put the request deadline of `AuthContext` into `BlockingGate::acquire` (gap 22); `UserRepo::set_password_hash_if` in the rehash path closes the race (gap 24); the principal cache TTL reads `Principal::expires_at` instead of parsing the JWT (gap 25).

New ports to consume (wave 2): `RevokedTokenRepo` replaces the in memory `RevokedTokenStore` of `internal_jwt` (move the trait out, keep an in memory impl for tests); `RobotTokenRepo::find_by_hash` as the second lookup after `TokenRepo::find_by_hash` in `TokenAuthenticator`, producing `AuthMethod::RobotToken` principals with `token_scopes` only, `last_used_at` coalesced; `ScannerDataRepo::find_scan_token` for a `ScanTokenAuthenticator` (`AuthMethod::ScanToken`, 15 minutes, one repository); session login refuses robot principals; `TokenRepo::{list_expiring, mark_expiry_notified}` for the expiry notice job.

### 3.5 rustryx-rbac (3 errors)

| File:line | What | Change |
|---|---|---|
| `src/rbac.rs:277`, `:359` | `PrivilegeGrant` matches | P2 (example there) |
| `src/state.rs:218` | `apply` match on `InvalidationEvent` | `Projects` drops the project caches (wave 2) and nothing else today; `Config(_)` is ignored; wildcard arm behaves like `Reset` |
| (P4) `tests/rbac_must_not.rs:536` | `forged.is_admin()` | `rbac.is_admin(&forged)` |
| (search) `tests/common/mod.rs`, `tests/selector_*.rs`, `tests/rbac_group.rs`, `tests/selector_corpus.rs`, `src/lib.rs` doc test, `src/scope.rs` | `ResourceRef`, `Coordinate`, `Repository` literals; `RepoFormat::ALL` in `tests/selector_limits.rs:318`, `tests/rbac_decisions.rs:38` | P1 and P2 |

Behaviour that `RbacChecker::check` must now own (gap 13, design 3.e decision order): anonymous callers are denied unless `resource.allow_anonymous` AND the anonymous switch of `RbacConfig` are both true; this moves out of the HTTP layer into `check`. Coordinates carry `extension` and `classifier`, so `coordinate.extension` and `coordinate.classifier` selectors compile (gap 12).

New methods to implement (wave 2, stream B): `RbacChecker::{is_admin, check_project, check_app, holds_whole_repository, visible_projects}` (all provided with a fail closed default today, so the crate compiles without them); the robot and scan token rule 4 of the decision order; project roles via `ProjectRole::template()` and the caches `direct` and `groups` of design 3.e; the `project:` scope production. Consumes `ProjectAccessRepo` (`memberships_of_user`, `group_mappings(provider, limit)`) and listens for `InvalidationEvent::Projects`.

### 3.6 rustryx-format-core (14 errors)

| File:line | What | Change |
|---|---|---|
| `src/error.rs:208`, `:214`, `:255`, `:261`, `:301`, `:307` | matches on `StoreError` and `BlobError` in three conversions (status, public code, log class) | map the new variants (P6): `StoreError::Invalid` is a client error 400, `BlobError::InvalidKey` 500 class, `InsufficientStorage` 507, `Overloaded` 503 with `Retry-After`, `Unsupported` 500 class; wildcard arm 500 class |
| `src/error.rs:394` | `From<DomainError>` match | map `Upstream` (502, 504 for `Timeout`) and `Violation` (`QuotaExceeded` 507, `ReadOnly` 503, the rest 409), wildcard arm `Internal` |
| `src/context.rs:435` | `AuditEvent` literal | `AuditEvent::new`, then assign |
| `src/engine.rs:288`, `src/group/mod.rs:223` | `ResourceRef` literals | `ResourceRef::for_repository(&repository, path)` everywhere authorization is built (mechanical). The request path has the `Repository` at hand |
| `src/hosted/upload.rs:226`, `:238`, `src/proxy/fill.rs:547`, `:560` | `NewComponent`, `NewAsset` literals | constructors, then assign `group_id`, `version`, `component_id`, `created_by` (the uploader id from the principal; never the client address) |
| (search) `src/request.rs`, `src/standard.rs`, `tests/support/mod.rs`, `tests/proxy.rs`, `tests/memory.rs` | `Coordinate`, `Repository`, `NewAsset`, `BlobMeta`, `BlobCommit` literals | constructors |

Also: the download path emits `ContentEvent::AssetDownloaded` only for `DownloadSource::Client` (`counts_as_download`); proxy cache fills call `publish_with` with `WriteOptions::new().with_mode(PublishMode::CreateOnly)` so a fill never replaces an artifact published meanwhile (gap 26) and use `touch_fresh` after a 304.

New items to consume (wave 2): `AssetRepo::{publish_with, link, delete_by_path_checked}`, `ComponentRepo::delete_checked`, `QuotaRepo::headroom` for the pre check before the body is read, `OutboxRepo::enqueue` (pull events through a bounded buffer), `RoutingRuleRepo` (the `RoutingGate` seam becomes a real port), `UpstreamError` for the proxy executor, `ProxyConfig::{registry_id, ca_certificate_ids, namespace_allowlist, bandwidth_limit_kbps}`; the new traits that live in this crate (`PullGate`, `LifecycleAdapter`, the `BlobStoreRegistry` change with `store_by_id` and `store_for_blob`, see design 4.g, 5.b and 7.b); the registry iterates `RepoFormat::ALL` as a slice.

### 3.7 rustryx-api (8 errors)

| File:line | What | Change |
|---|---|---|
| `src/auth.rs:151` | `AuthContext` literal | `AuthContext::new().with_client_ip(..).with_request_id(..).with_deadline(..)` (use the request deadline: gap 22) |
| `src/auth.rs:196` | `method_label` match on `AuthMethod` | `RobotToken => "robot"`, `ScanToken => "scan"`, wildcard arm `"unknown"` |
| `src/auth.rs:208` | `failure_label` match on `AuthError` | `AccessDenied(_)` is `"denied"`; wildcard arm `"error"` |
| `src/error.rs:472`, `:492`, `:512` | `From<StoreError>`, `From<BlobError>`, `From<AuthError>` | P6 table; `StoreError::Invalid` is 400 `invalid_input` (or `invalid_page_token`), `AuthError::AccessDenied` 403 `forbidden` |
| `src/error.rs:559` | `From<DomainError>` | `Upstream` and `Violation` (P6), wildcard arm 500 |
| `src/test_support.rs:167` | `ResourceRef` literal | `for_repository` |

Also: use `authenticate_detailed` so a failure counts with its method; add the lint test that no service message of `Invalid`, `Conflict` or `AlreadyExists` carries internals (gap 31) and the test that no response type embeds `Secret` (gap 34; use `SecretStatus` for write only secrets); the contract conflicts of docs/api-conventions.md section 19 rows 1, 4, 8, 9, 11, 13, 15 are closed by this revision (RFC 3339, `StoreError::Invalid`, `Principal` fields, `RbacChecker::is_admin`, `AuditReadRepo`, `update` on blob stores, `Coordinate`).

New items to consume or implement (wave 2): everything of design sections 3 to 8 behind `/api/v1`: `ProjectRepo` and friends, labels, robots, quotas, lifecycle, scanning, registries, replication, webhooks, promotion, storage, settings; the `Requires` vocabulary of section 2.8 with `RbacChecker::{check_project, check_app, is_admin}`; the `SettingsSnapshot` and `SettingsSource` handle (`SettingsSection::key` and `secret_pointers` tell it what to read and seal); the status check runner for `StatusCheckProvider` (5 second budget, `Unknown` on a timeout); the write middleware for read only mode (503, `PolicyViolation::ReadOnly`); `gate_open` before creating gated data (`409 cluster_upgrade_in_progress` with `nodes_below_level`).

### 3.8 rustryx-server (not reached)

It compiles against `rustryx-api`, `rustryx-config`, `rustryx-core` and `rustryx-observability`. Expect only what `rustryx-api` hands down: no direct use of the changed core items was found by search. Wave 2 wiring (design 1.6 step 2 onwards): build the new repositories and the `NodeRegistryRepo` heartbeat (`CURRENT_FEATURE_LEVEL`), pass `Arc<dyn RevokedTokenRepo>` to the auth service, register the status check providers and the task handlers, create the stub crates `rustryx-egress`, `rustryx-scan`, `rustryx-replication`, `rustryx-notify`, `rustryx-cli`.

### 3.9 rustryx-config (0 errors)

No breaking change. Wave 2 adds keys (design 2.5): `auth.oidc.authorized_parties` (default `[client_id]`, gap 17), `egress.registries.*`, `egress.webhooks.*`, `egress.scanners.*`, `egress.smtp.*`, `egress.proxy.allowed_hosts`, `egress.probe.*`, `tasks.max_concurrent_runs` (4), `tasks.scheduler_tick_secs` (15), `tasks.run_retention_days` (90), `tasks.log_retention_days` (30), `outbox.retention_days` (30), `outbox.poll_interval_ms` (1000), `downloads.flush_interval_secs` (10), `downloads.retention_days` (90, at least 35), `scan.*`, `notify.*`, `replication.*`, `webhooks.suspend_after_failures` (100). Each needs the key table entry, the bless step of config.md and `config.example.yaml`. The LDAP defaults noted in core-gaps (`groups_cache_ttl_secs` 60) are config work, not core.

### 3.10 rustryx-observability (0 errors)

No breaking change. Wave 2: the metric names of design 2.3 (`background_job_*`, with `result="skipped"`), 3.d, 4.d, 5.d, 6.d, 7.d, 8.d, `LogControl` over `tracing_subscriber::reload` for `settings.logging`, and the redactor must scrub `X-Amz-Signature` and the Azure `sig` parameter (presigned URLs). The label rules of design 2.6 apply (no project, repository, user, path, scanner name or webhook id in a label).

### 3.11 Crates that do not change

`rustryx-tasks` and `rustryx-format-{maven,npm,pypi,oci,helm,raw,pub}` compile unchanged. `rustryx-tasks` gets the framework of design 2.3 (it consumes `TaskDefinitionRepo`, `TaskRunRepo`, `LockBackend`, `JobLeaseRepo`, `OutboxRepo`); the format crates will consume `AssetRepo::{publish_with, link}`, `LifecycleAdapter` and the OCI catalog contract of design 4.g when they land.

## 4. Migration 0007 (wave2 foundation)

`rustryx-db` writes `0007_wave2_foundation.sql` from design section 1.5 (the full SQL is there; this is the checklist). It is expand only, applied with Core revision 2, and nothing in it is read by an N-1 binary.

| Object | Content |
|---|---|
| `projects` | `id`, `name` (CHECK `^[a-z0-9]+([._-][a-z0-9]+)*$` and length 2 to 64, the rule of `ProjectName`), `description` (at most 4000), `visibility` (`private` or `public`), `created_by`, `version`, times, `UNIQUE (name)` |
| `repositories` | column `project_id` (`ON DELETE RESTRICT`), partial index `ix_repositories_project`; trigger `repositories_project_sync` (BEFORE INSERT OR UPDATE OF `project_id`, `allow_anonymous`: `allow_anonymous` follows the project visibility); trigger function `projects_visibility_sync` on `projects` (AFTER UPDATE OF `visibility`: rewrites `allow_anonymous` of the project's repositories and `pg_notify('rustryx_rbac', 'projects')`) |
| `components` | columns `created_by`, `last_downloaded_at` |
| `assets` | column `created_by` |
| `audit_events` | columns `actor_kind` (NOT NULL DEFAULT `'user'`) and `project_id`; index `ix_audit_events_project (project_id, event_time DESC) WHERE project_id IS NOT NULL` |
| `system_config` | sequence `system_config_revision_seq`; columns `revision` (default `nextval`) and `updated_by`; trigger `system_config_bump` (BEFORE UPDATE: new revision, `updated_at`) |
| `revoked_tokens` | `tid uuid PRIMARY KEY`, `expires_at`, index on `expires_at` (backs `RevokedTokenRepo`) |
| `node_heartbeats` | `node_id` (CHECK non empty, at most 200), `version`, `feature_level` (at least 1), `started_at`, `seen_at`, `settings_revision`, `inflight_writes`, `details jsonb` (backs `NodeRegistryRepo`) |
| `event_outbox` | partitioned by range on `occurred_at`; `seq` from `event_outbox_seq`, `tx_id xid8 DEFAULT pg_current_xact_id()`, `id`, `kind`, `project_id`, `repository_id`, `actor_id`, `actor_name`, `actor_kind`, `request_id`, `data jsonb`; PRIMARY KEY `(occurred_at, seq)`; indexes `ix_event_outbox_relay (tx_id, seq)` and unique `(id, occurred_at)` |
| `outbox_cursors` | `consumer` PK, `last_tx_id xid8`, `last_seq`, `updated_at` |
| functions | `ensure_outbox_partitions(now, lookahead)` and `drop_old_outbox_partitions(retention_days)`, written like the audit functions of 0005 (generated names only, advisory transaction lock) |
| `task_definitions` | `kind` (CHECK `^[a-z][a-z0-9_]{0,63}$`), `name` (unique), `params`, `schedule`, `enabled`, `system`, `notify`, `next_run_at`, `last_run_id`, `version`, times; partial index `ix_task_definitions_due` |
| `task_runs` | `status` CHECK (`queued, running, succeeded, failed, cancelled, skipped, abandoned`, the wire names of `TaskRunStatus`), `trigger` CHECK (`schedule, manual, event, startup`, `TaskTrigger`), `dry_run`, `params`, node, lease and heartbeat columns, `stop_requested_at`, `summary`, `error` (at most 500), `attempts`; UNIQUE partial index `uq_task_runs_active ON (kind, scope_key) WHERE status IN ('queued', 'running')`; indexes for task history and claim |
| `task_run_log` | `(run_id, seq)` PK, `at`, `level smallint`, `message` (at most 1024), cascade from `task_runs` |

Not in 0007 on purpose: `routing_rules` and `cleanup_policies` (their owners add them in their own range), everything stream specific. Alongside the SQL, `rustryx-db` implements what the migration serves: `map_err` for `RX004` to `RX008`, the `Projects` and `Config` bus payloads, `audit_events` writes for the two new columns, the repositories for `RevokedTokenRepo`, `NodeRegistryRepo`, `OutboxRepo`, `TaskDefinitionRepo` and `TaskRunRepo` (C0), `SystemConfigRepo`.

## 5. Things the design document left open

Choices made in core. A crate owner can rely on them; each is covered by a test in `rustryx-core`.

| Topic | Choice |
|---|---|
| `ProjectName` | uppercase input is rejected, not lowered (an OCI path component is lowercase and a silent rewrite would make two names look like one) |
| `PullGatePolicy` threshold | `Negligible` and `Unknown` are both refused as a threshold (either would block everything); `PullGatePolicy::default()` is `High`, `AuditOnly`, unscanned denied in `Enforce` |
| vulnerability ids | `normalize_vuln_id` upper cases every id (the design text calls GHSA ids "as published" and also "normalized upper case"; the allowlist column requires `vuln_id = upper(vuln_id)`) |
| `ProxyConfig::new` defaults | content 3600 s, metadata 300 s, negative 60 s, 8 concurrent (the design says "3 s, 300 s, 60 s and 8", which does not map onto four fields) |
| `AssetRepo` required methods | five required (`publish_with`, `link`, `delete_by_path_checked`, `touch_fresh`, `record_downloads`), `publish` provided, as design section 1.4 lists them (the comment in 1.3.8 says "three required, two provided") |
| `BlobRepo::move_blob`, `list_page_by_store` | provided with a fail closed default, because section 1.4 does not list them among the new required methods |
| `RobotScopeSet` | `read` needs `browse` and `write` or `delete` need `read` on the SAME target; a project id in a scope is the canonical lowercase hyphenated UUID; exact duplicates collapse |
| `RoutingRule` | needs at least one pattern (a rule without one matches nothing) |
| `TrustPolicy::validate` | a required scheme needs at least one key, root or store, so "a signature exists" can never be the whole policy |
| outbox hand-off | a repository method that takes `advance` and loses the compare and set writes nothing and returns 0; the caller reads the next batch from the new cursor |
| `UploadHandle` | defined in core (the design names it without defining it): 1 to 128 bytes of `A-Z a-z 0-9 . _ ~ -`; a malformed handle is `BlobError::NotFound` |
| `NewRobotToken`, `ReadOnlyState`, the five small settings sections | defined in core (named in the manifest or the design, never defined) |
| `RoutingRuleRepo` | lives in `routing.rs` as the design block has it and is re-exported as `rustryx_core::ports::RoutingRuleRepo` |
| file layout | `scan`, `lifecycle` and `settings` are directories with the same public paths (`rustryx_core::scan::X`); tests of the larger modules sit in `<module>/tests.rs` |
