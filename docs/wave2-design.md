# Rustryx wave 2 design: data model and contracts

Status: design draft, 2026-10-24. Owner: architecture. Inputs: [parity-compat-harbor.md](parity-compat-harbor.md) sections 6 to 14, [core-gaps.md](core-gaps.md), [db.md](db.md), [rbac.md](rbac.md), [auth-core.md](auth-core.md), [format-core.md](format-core.md), [server.md](server.md), [config.md](config.md), [slo.md](slo.md), the `rustryx-core` sources, and the Russian specification (README section 8 wins over every role section).

This document fixes the data model and the contracts of the wave 2 backend streams W2-B, W2-C, W2-D, W2-E, W2-F and W2-H, so that the streams can be built in parallel by different people. It is a design: nothing here has been compiled, and every statement about a third party tool that was not verified is marked `unverified` or listed as a spike in section 10. Vendor behaviour is described only as far as the parity document already summarizes it; nothing is copied from vendor documentation.

Contents:

1. [Core revision 2](#1-core-revision-2): one consolidated change list for `rustryx-core`, the gap dispositions, the breaking changes, the foundation migration.
2. [Cross-cutting mechanisms](#2-cross-cutting-mechanisms): secrets at rest, event outbox, task framework and locks, feature levels (N-1 safety), egress, metrics, migration ranges.
3. [W2-B](#3-w2-b-projects-members-roles-robot-tokens-labels) projects, members, roles, robot tokens, labels.
4. [W2-C](#4-w2-c-lifecycle-and-capacity) lifecycle and capacity.
5. [W2-D](#5-w2-d-supply-chain) supply chain.
6. [W2-E](#6-w2-e-distribution) distribution.
7. [W2-F](#7-w2-f-storage) storage.
8. [W2-H](#8-w2-h-platform-settings) platform settings.
9. [Negative requirement index](#9-negative-requirement-index)
10. [Spikes](#10-spikes-what-each-must-prove)
11. [ADR table](#11-adr-table)
12. [Implementation order and sizes](#12-implementation-order-and-sizes)
13. [Open questions for the product owner](#13-open-questions-for-the-product-owner)

## 0. Conventions used by every section

| Topic | Rule |
|---|---|
| Identifiers | `id_type!` newtypes over `UUIDv7` (see `ids.rs`). A new entity gets a new id type, never a bare `Uuid`. A node id is the one exception: `String` from config or hostname. |
| Time and sizes | `time::OffsetDateTime` in UTC. Durations in entities are whole seconds named `*_secs`. Sizes are `u64` bytes named `*_bytes`. |
| serde | `rename_all = "snake_case"` on enums and structs. The one documented exception is `EventKind`, whose wire names are dotted (`scan.completed`) because the specification (DSO-32) fixes them. |
| Secrets | `Secret` for values Rustryx must send out (webhook secret, registry password, SMTP password). A value Rustryx only verifies is stored as a SHA-256 hash (tokens). Response DTOs never embed `Secret`; they carry `SecretStatus { set: bool }` (section 1.3.1). |
| Optimistic concurrency | Every admin editable entity carries `version: i64`. Update ports take `expected_version` and return `StoreError::Conflict` on mismatch. The HTTP layer maps the `If-Match` header to it (UI inventory rule 8). |
| Ports | Same contract as `ports/mod.rs`: typed errors, each method one statement or one transaction, cancel safe unless the doc line says otherwise. Ports that enforce a policy return `MutationError` (section 1.3.1). |
| Tags in code blocks | A fenced block whose first line is `// core: <path>` is the exact text the lead copies into `rustryx-core` (new file or appended section). `// crate: <name>` blocks belong to that crate and are not part of Core revision 2. |
| Test names | A requirement of the form "must never" is a test named `must_not_*`, as in db.md section 12. Each stream lists its own in the `h` part. |
| No dashes | This document contains no em dash and no en dash. Ranges are written with "to". |

Stream part letters used below: (a) entities and enums, (b) ports, (c) PostgreSQL, (d) jobs, (e) authorization, (f) events, (g) crates, (h) negative requirements.

---

## 1. Core revision 2

### 1.1 Rules of the revision

1. One revision, applied by the lead in one commit (order in section 1.6). Crate owners adapt in the same pull request sequence. The list of crates per breaking change is in section 1.4.
2. After this revision every public entity struct of `rustryx-core` that wave 2 touches is `#[non_exhaustive]` with a constructor, and every public enum that can grow is `#[non_exhaustive]`. The one time cost of the literal breakage is paid here, so adding a field or a variant later is not breaking. Fields stay `pub`, so callers can still read and assign them; only struct literals and exhaustive matches change.
3. A new method on an existing trait is a provided method with a fail closed default wherever a default can be safe (`Ok(false)`, `Deny`, `Unsupported`). A method is required only where a default would hide a security or consistency bug. Required methods break the implementers (`rustryx-db`, `rustryx-testkit`, blob stores), and those are the crates that own the work anyway.
4. `rustryx-core` gets no new dependency. Deadlines use `std::time::Instant`. Cancellation (`CancellationToken`) lives in `rustryx-tasks`, which is why the `TaskHandler` trait is not in core.
5. A policy that is enforced in SQL (immutability, holds, read-only, quota) has a typed Rust error (`PolicyViolation`) and a SQLSTATE: `RX004` immutable, `RX005` held, `RX006` quota exceeded, `RX007` read-only, `RX008` scope violation. `rustryx-db::error::map_err` maps them to `StoreError::Conflict` for the old methods and to `MutationError::Violation` for the new ones.
6. Stream specific entities and ports are defined in the stream sections (blocks tagged `// core:`). Section 1.3.10 is the manifest that lists every file and trait, so the lead can create all of them in the same commit. They are pure additions: no existing code references them, so they compile without implementers and every stream can start against them.

### 1.2 Disposition of the 34 gaps of core-gaps.md

Dispositions: ADOPT (as proposed), ADOPT-MODIFIED (same goal, different change), REJECT (with the reason), DEFER (to a named owner), CLOSED (already closed).

| # | Disposition | Change in revision 2 | Reason |
|---|---|---|---|
| 1 | ADOPT | `StoreError::Invalid(String)` | a malformed token must not look like a conflict; db and testkit switch to it |
| 2 | ADOPT | `BlobRepo::insert(&BlobRecord)` required | tests and the GC reconciliation seed rows through the port instead of around it |
| 3 | ADOPT | module doc of `ports/repos.rs`: deleting a missing row of an admin facing entity is `NotFound`, GC facing calls are idempotent, role and privilege delete cascades to link tables; testkit gets a conformance test per rule | db.md section 5 already behaves this way, the port never said so |
| 4 | CLOSED | none | consumers use `Arc<dyn Trait>` |
| 5 | ADOPT | `BlobError::InvalidKey(&'static str)` | a bad key is a caller bug and must not read as `NotFound` |
| 6 | ADOPT | `BlobError::InsufficientStorage` | SRE-34 needs 507 |
| 7 | ADOPT | `BlobError::Overloaded { retry_after: Option<Duration> }` | 503 with `Retry-After` needs the hint |
| 8 | ADOPT | `BlobStore::shutdown(&self)` provided, no-op | removes the side trait `CleanupDrain` |
| 9 | ADOPT-MODIFIED | fix the `BlobMeta` doc (size only) and add `modified: Option<OffsetDateTime>` | S3 returns no SHA-256 on `HEAD`, so promising digests is wrong; the modification time is what the staging sweep and `verify-blobs` need |
| 10 | ADOPT | `BlobStoreRepo::update(&BlobStoreDef)` | groups, soft quota and encryption are edited after creation; the registry cache is told through `invalidate` |
| 11 | ADOPT-MODIFIED | `BlobStore::capabilities() -> BlobStoreCapabilities` provided, with `lists_staging`, `lists_keys`, `can_presign`, `resumable` | an `Option` return would break the signature of a trait that two crates implement; a capability struct says "empty because unsupported" without breaking anything |
| 12 | ADOPT | `Coordinate { extension, classifier }` | maven selectors use `coordinate.extension` |
| 13 | ADOPT-MODIFIED | `ResourceRef.allow_anonymous` (mandatory in the constructor) and `RbacConfig`'s anonymous switch handle inside `Rbac`; `Rbac::check` denies anonymous callers unless both are true | makes the two-switch rule structural: a caller that forgets does not compile, and `check` fails closed. A test in api alone would still allow a forgotten call |
| 14 | ADOPT | `RbacChecker::is_admin` provided, default `Ok(false)` | admin routes need a trait level answer; the default is safe |
| 15 | ADOPT | remove `Principal::is_admin()`; `has_role` stays and its doc says "name based, untrusted, for display and tests" | the name based method is a footgun; production code never calls it (only tests do, section 1.4) |
| 16 | ADOPT | `Principal.email`, `display_name` | JIT creates accounts with an e-mail; the cache TTL no longer parses the JWT |
| 17 | ADOPT-MODIFIED | not core: `auth.oidc.authorized_parties` in `rustryx-config`, default `[client_id]`; operators add `rustryx-cli` | the strict `azp` rule stays; the second client is an explicit allowlist entry, never implicit |
| 18 | ADOPT | same as 1 | same |
| 19 | ADOPT | ports `AuditReadRepo`, `JobLeaseRepo`, `SystemConfigRepo`, `CleanupPolicyRepo`, `RoutingRuleRepo` (signatures in sections 1.3.9, 4.b, 8.b) | every wave 2 job and the UI need them; db keeps the inherent APIs as the implementation |
| 20 | ADOPT | `AuthError::AccessDenied(&'static str)` (403) | "no mapped group" and "not provisioned" are not `GroupLookupFailed` or `AccountDisabled` |
| 21 | ADOPT | `RevokedTokenRepo` port plus table `revoked_tokens` in migration 0007 | an in-memory revocation list is per node and breaks revocation in HA |
| 22 | ADOPT | `AuthContext.deadline: Option<std::time::Instant>` | the gate then honours the request deadline |
| 23 | ADOPT | `ExternalGroups.confirmed_at: OffsetDateTime` (name from ldap.md) | the 5 minute window is timed from the directory's answer, not from receipt |
| 24 | ADOPT | `UserRepo::set_password_hash_if(id, old_hash, new_hash) -> bool` required | closes the rehash race |
| 25 | ADOPT | `Principal.external_id`, `expires_at` (with 16) | LDAP refresh needs the DN; the token expiry bounds the principal cache |
| 26 | ADOPT | `PublishMode`, `WriteOptions`, `AssetRepo::publish_with`, `link`, `touch_fresh` | race free `allow_once` across nodes and cheap cache revalidation |
| 27 | ADOPT | `RoutingRule` entity and `RoutingRuleRepo` (section 1.3.9) | `RoutingGate` becomes a real port |
| 28 | REJECT | no pending blob row | an extra insert and update on every upload, and it would not cover a lost S3 commit response. The store sweep in `verify_blobs` (section 7) finds both leaks and is needed for `BLOB-19` anyway; it needs `BlobStore::list_keys` |
| 29 | ADOPT-MODIFIED | optional capability `BlobStore::resumable()` (section 1.3.7) with the contract only; the implementation belongs to the OCI crate and the blob crate | an S3 multipart upload id is shared state, so any node can continue it, but parts under 5 MiB are illegal, so the contract stages chunks and re-reads them at finalize to hash (a SHA-256 state cannot be moved between nodes) |
| 30 | ADOPT-MODIFIED | `ProxyConfig.ca_certificate_ids: Vec<TrustedCertificateId>` instead of `ca_bundle_secret_ref` | a certificate is public data, not a secret; the managed trust store (section 8) gives audit and expiry monitoring |
| 31 | ADOPT | doc rule on `DomainError::{Invalid, Conflict, AlreadyExists}` ("message is public") plus a lint test in `rustryx-api` over every service message | changing the variants to a newtype is churn for no gain |
| 32 | ADOPT | `AuthFailure { method, error }` and provided `Authenticator::authenticate_detailed` | additive; the chain overrides it so `auth_attempts_total` gets a real method |
| 33 | ADOPT | `UpstreamError` and `DomainError::Upstream` | one typed 502 and 504 mapping for format-core, api, scan, replication and notify |
| 34 | ADOPT-MODIFIED | `Secret` keeps serializing its value (persistence needs it); add `SecretStatus` for DTOs; a test in each API crate forbids `Secret` fields in response types | changing `Serialize` would break the sealing paths |

Items that are not in the table because they were not core gaps but are decided here: `InvalidationEvent::User(Uuid)` stays a `Uuid` (rbac gap 4 is cosmetic, the break is not worth it); `RepoFormat` becomes `#[non_exhaustive]` and `ALL` becomes a slice so the phase 2 formats of W2-G can land without another break.

### 1.3 The consolidated change list

#### 1.3.1 `error.rs`, `secret.rs`

```rust
// core: crates/rustryx-core/src/error.rs  (replaces StoreError, BlobError, AuthError, DomainError; adds the rest)
use std::time::Duration;

use crate::ids::{ImmutabilityRuleId, LifecycleHoldId};
use crate::model::WritePolicy;
use crate::quota::QuotaExceeded;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Malformed input such as a bad page token or an out of range number (gaps 1, 18).
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("store unavailable: {0}")]
    Unavailable(String),
    #[error("store backend failure during {context}")]
    Backend { context: &'static str, #[source] source: BoxedSource },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlobError {
    #[error("blob not found")]
    NotFound,
    #[error("blob store unavailable: {0}")]
    Unavailable(String),
    #[error("range not satisfiable")]
    RangeNotSatisfiable,
    #[error("blob writer is closed")]
    WriterClosed,
    /// Caller bug: the key could escape the store root (gap 5).
    #[error("invalid blob key: {0}")]
    InvalidKey(&'static str),
    /// Local disk is full or below the free space floor, maps to 507 (gap 6).
    #[error("insufficient storage")]
    InsufficientStorage,
    /// No upload slot or similar, maps to 503 with Retry-After (gap 7).
    #[error("blob store overloaded")]
    Overloaded { retry_after: Option<Duration> },
    /// The store cannot do this (list keys, presign, resumable upload).
    #[error("operation not supported by this blob store: {0}")]
    Unsupported(&'static str),
    #[error("blob backend failure during {context}")]
    Backend { context: &'static str, #[source] source: BoxedSource },
}

// AuthError: add one variant (gap 20), mark #[non_exhaustive]. All other variants unchanged.
//     /// No mapped group and no default role, or the identity may not be provisioned. Maps to 403.
//     #[error("access denied: {0}")]
//     AccessDenied(&'static str),

/// What an upstream (proxy target, registry, scanner endpoint, webhook target) did wrong (gap 33).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpstreamError {
    #[error("upstream timed out")]
    Timeout,
    #[error("upstream connection failed")]
    Connect,
    #[error("upstream tls failure")]
    Tls,
    #[error("upstream answered {0}")]
    Status(u16),
    #[error("upstream body too large")]
    TooLarge,
    /// The egress policy (SSRF filter) refused the target. The reason is a fixed label.
    #[error("target refused by egress policy: {0}")]
    Blocked(&'static str),
    #[error("upstream protocol error")]
    Protocol,
    #[error("upstream circuit open")]
    CircuitOpen,
}

impl UpstreamError {
    /// 504 for `Timeout`, 502 for everything else.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self { Self::Timeout => 504, _ => 502 }
    }
}

/// A policy said no. These are expected outcomes, not store failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyViolation {
    #[error("item is immutable")]
    Immutable { rule: ImmutabilityRuleId },
    #[error("item is under a hold")]
    Held { hold: LifecycleHoldId },
    #[error("quota exceeded")]
    QuotaExceeded(QuotaExceeded),
    /// `Deny`, or `AllowOnce` on an existing path.
    #[error("write policy forbids this write")]
    WritePolicy(WritePolicy),
    /// `PublishMode::CreateOnly` found the path taken.
    #[error("path already exists")]
    AlreadyExists,
    #[error("read only mode")]
    ReadOnly,
    #[error("promotion approval required")]
    ApprovalRequired,
}

/// Error of a port method that enforces policies inside its transaction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MutationError {
    #[error(transparent)]
    Violation(#[from] PolicyViolation),
    #[error(transparent)]
    Store(#[from] StoreError),
}

// DomainError: mark #[non_exhaustive], add
//     #[error(transparent)] Upstream(#[from] UpstreamError),
//     #[error(transparent)] Violation(#[from] PolicyViolation),   // QuotaExceeded keeps mapping to 507,
//                                                                 // ReadOnly to 503 `read_only`, the rest to 409
// and `impl From<MutationError> for DomainError`.
// Doc rule (gap 31): the messages of Invalid, Conflict and AlreadyExists reach clients verbatim.
```

```rust
// core: crates/rustryx-core/src/secret.rs  (append)
/// What a response DTO says about a stored secret instead of the secret (gap 34).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretStatus {
    /// True when a value is stored.
    pub set: bool,
}

impl From<&Option<Secret>> for SecretStatus {
    fn from(value: &Option<Secret>) -> Self { Self { set: value.is_some() } }
}
```

#### 1.3.2 `ids.rs`

All new identifier types of wave 2 in one block (usage is in the stream sections). Bare `id_type!` lines, same macro as today.

```rust
// core: crates/rustryx-core/src/ids.rs  (append)
id_type!(/** Identifier of a project. */ ProjectId);
id_type!(/** Identifier of a project member row. */ ProjectMemberId);
id_type!(/** Identifier of a label. */ LabelId);
id_type!(/** Identifier of a robot token record. */ RobotTokenId);
id_type!(/** Identifier of an immutability rule. */ ImmutabilityRuleId);
id_type!(/** Identifier of a retention policy. */ RetentionPolicyId);
id_type!(/** Identifier of a lifecycle run (cleanup, retention, label cleanup). */ LifecycleRunId);
id_type!(/** Identifier of a lifecycle hold. */ LifecycleHoldId);
id_type!(/** Identifier of a garbage collection run. */ GcRunId);
id_type!(/** Identifier of a task definition. */ TaskDefinitionId);
id_type!(/** Identifier of one run of a task. */ TaskRunId);
id_type!(/** Identifier of an event (stable across delivery attempts). */ EventId);
id_type!(/** Identifier of a scanner registration. */ ScannerId);
id_type!(/** Identifier of a scan report. */ ScanReportId);
id_type!(/** Identifier of a CVE allowlist entry. */ CveAllowlistEntryId);
id_type!(/** Identifier of a scan or gate policy row. */ SecurityPolicyId);
id_type!(/** Identifier of a trust policy. */ TrustPolicyId);
id_type!(/** Identifier of a piece of trust material (key, root, store). */ TrustMaterialId);
id_type!(/** Identifier of a scanner data set version. */ ScannerDataSetId);
id_type!(/** Identifier of an export (CSV and similar). */ ExportId);
id_type!(/** Identifier of a registry endpoint. */ RegistryId);
id_type!(/** Identifier of a replication rule. */ ReplicationRuleId);
id_type!(/** Identifier of a replication execution. */ ReplicationExecutionId);
id_type!(/** Identifier of a replication task. */ ReplicationTaskId);
id_type!(/** Identifier of a webhook. */ WebhookId);
id_type!(/** Identifier of a webhook delivery. */ WebhookDeliveryId);
id_type!(/** Identifier of a notification subscription. */ SubscriptionId);
id_type!(/** Identifier of an e-mail delivery. */ EmailDeliveryId);
id_type!(/** Identifier of a promotion request. */ PromotionId);
id_type!(/** Identifier of a trusted certificate. */ TrustedCertificateId);
id_type!(/** Identifier of a blob store verification run. */ VerifyRunId);
```

#### 1.3.3 `model.rs`

Changed types, shown with the delta only. Every struct below becomes `#[non_exhaustive]` and gets the constructor listed.

```rust
// core: crates/rustryx-core/src/model.rs  (changes)

// RepoFormat: #[non_exhaustive]; `pub const ALL: &'static [Self]` (was [Self; 8]); phase 2 formats are added
// by W2-G without a break.

/// Link from a repository to its project, with what authorization needs (W2-B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: ProjectId,
    pub name: ProjectName,
    pub visibility: ProjectVisibility,
}

// Repository: one new field. For a repository in a public project `allow_anonymous` is true and for one in a
// private project it is false; the database holds that invariant by trigger (section 3.c).
//     pub project: Option<ProjectRef>,
impl Repository {
    /// Defaults: `WritePolicy::Allow`, `allow_anonymous` false, `online` true, no quota, no policies,
    /// no routing rule, no proxy or group config, empty `format_config`, no project, both times `now`.
    pub fn new(id: RepositoryId, name: RepoName, format: RepoFormat, repo_type: RepoType,
               blob_store_id: BlobStoreId, now: OffsetDateTime) -> Self;
}

// ProxyConfig: two new fields (gap 30 and W2-E registries); constructor sets the 3 s, 300 s, 60 s and 8 defaults
// of format-core Limits for the numeric fields.
//     /// When set, `remote_url` and `auth` are resolved from this registry by the repository port.
//     pub registry_id: Option<RegistryId>,
//     /// Extra CA certificates for this upstream, from the managed trust store.
//     pub ca_certificate_ids: Vec<TrustedCertificateId>,
impl ProxyConfig {
    pub fn new(remote_url: impl Into<String>) -> Self;
}

// BlobStoreKind: #[non_exhaustive], new variants `Azure` and `Group` (W2-F).
// BlobStoreDef: new fields (W2-C soft quota, W2-F group).
//     pub soft_quota: Option<SoftQuota>,
//     pub group: Option<BlobGroupConfig>,
impl BlobStoreDef {
    pub fn new(id: BlobStoreId, name: impl Into<String>, kind: BlobStoreKind,
               config: serde_json::Value, now: OffsetDateTime) -> Self;
}

// Component: two new fields (W2-C pull time, W2-E promotion approver rule).
//     pub created_by: Option<UserId>,
//     pub last_downloaded_at: Option<OffsetDateTime>,
// NewComponent: `pub created_by: Option<UserId>`; both get `::new(..)`.
// Asset: `pub created_by: Option<UserId>` (uploader id, never the client address by default).
// NewAsset: `pub created_by: Option<UserId>`; constructor
impl NewAsset {
    pub fn new(repository_id: RepositoryId, path: impl Into<String>,
               content_type: impl Into<String>, blob: BlobRecord) -> Self;
}

/// What a publish may do to an existing path (gap 26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublishMode {
    /// Create or replace, subject to the repository write policy (default).
    #[default]
    Upsert,
    /// Create only. An existing path is `PolicyViolation::AlreadyExists`. Proxy cache fills use it so a fill never
    /// replaces an artifact published meanwhile, across nodes.
    CreateOnly,
}

/// Options of a state changing port call. `#[non_exhaustive]`, built with `WriteOptions::new()` and `with_*`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    pub mode: PublishMode,
    /// Only an `nx-admin` driven lifecycle or admin run sets this. It sets the transaction local setting
    /// `rustryx.override_immutable`, which the SQL guards honour. Never set from request data.
    pub override_immutable: bool,
    /// Who acts, for events and for the audit batch row. `None` means the system.
    pub actor: Option<ActorRef>,
    pub request_id: Option<String>,
    /// Events written to the outbox in the same transaction (section 2.2).
    pub events: Vec<EventDraft>,
}

// PublishOutcome: #[non_exhaustive], new field `pub created: bool` (false when an existing path was replaced).

/// Publish an asset that points at an already stored blob (promotion, mount, cross repository copy).
/// Both repositories must use the same blob store, otherwise `StoreError::Conflict` and the caller copies bytes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewAssetLink {
    pub repository_id: RepositoryId,
    pub component_id: Option<ComponentId>,
    pub path: String,
    pub content_type: String,
    pub blob_id: BlobId,
    pub created_by: Option<UserId>,
}

/// One batched download observation (W2-C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadTick {
    pub asset_id: AssetId,
    pub at: OffsetDateTime,
    pub count: u32,
}

/// Who caused a read. Only `Client` increments counters and refreshes pull times (SCN-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadSource { Client, Scanner, Replication, Internal }
```

#### 1.3.4 `identity.rs`

```rust
// core: crates/rustryx-core/src/identity.rs  (changes)

// AuthMethod: #[non_exhaustive]; new variants
//     /// A robot token (`robot_tokens`), W2-B. Rights come from its scopes only.
//     RobotToken,
//     /// A short lived scan access token handed to an external scanner, W2-D. Exempt from the pull gate and counters.
//     ScanToken,

// Principal: #[non_exhaustive]; four new fields (gaps 16, 25), constructor, `is_admin()` removed (gap 15).
//     pub email: Option<String>,
//     pub display_name: Option<String>,
//     /// Directory identity that lets a refresh skip the user search (LDAP DN), `None` otherwise.
//     pub external_id: Option<String>,
//     /// Expiry of the credential that produced this principal (JWT `exp`, token `expires_at`).
//     pub expires_at: Option<OffsetDateTime>,
impl Principal {
    /// Everything else empty or `None`; `roles` and `external_groups` are filled by the chain.
    pub fn new(username: impl Into<String>, source: UserSource, auth_method: AuthMethod) -> Self;
    /// The kind of actor, for events and audit: `Anonymous`, `Robot` for `RobotToken`/`ScanToken`,
    /// `Token` for `ApiToken`, `User` otherwise.
    pub fn actor_kind(&self) -> ActorKind;
}

// PrivilegeGrant: #[non_exhaustive]; one new variant (AUTH-46, used from W2-H on; see section 8.e).
//     /// An administration area. `Action::Read` is "read", `Write` is "create and update", `Delete` is "delete".
//     Application { area: AppArea },
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppArea {
    Users, Roles, Privileges, Selectors, Mappings, Tokens, Blobstores, Tasks, Settings,
    Audit, Scanners, Registries, Replication, Webhooks, Quotas, Gc, System,
}

// ExternalGroups (ports/auth.rs): #[non_exhaustive], new field `pub confirmed_at: OffsetDateTime` (gap 23),
// constructor `ExternalGroups::new(provider, groups, confirmed_at)`.
// AuthContext: #[non_exhaustive], new field `pub deadline: Option<std::time::Instant>` (gap 22);
// `AuthContext::new()` plus `with_client_ip`, `with_request_id`, `with_deadline`.
// Coordinate: #[non_exhaustive], new fields `extension`, `classifier` (gap 12); `Coordinate::new(group, name, version)`.

/// An authenticator failure with the method that refused (gap 32).
#[derive(Debug)]
#[non_exhaustive]
pub struct AuthFailure {
    pub method: Option<AuthMethod>,
    pub error: AuthError,
}
```

#### 1.3.5 `audit.rs`

```rust
// core: crates/rustryx-core/src/audit.rs  (changes)

// AuditKind: #[non_exhaustive]; new variants (the db text mapping must accept them, the column is plain text)
//     ProjectChange,      // project, member, label, visibility
//     PolicyChange,       // retention, immutability, quota, allowlist, trust, gate, hold
//     SecurityDecision,   // pull blocked, immutable reject, quota reject, SSRF reject, quarantine
//     LifecycleAction,    // batch deletions of cleanup, retention, promotion, gc
//     SettingsChange,     // settings sections, read only mode, trust store, log levels
//     IntegrationChange,  // webhook, registry, replication rule, scanner
//     PromotionChange,    // request, approval, execution

// AuditEvent: #[non_exhaustive]; new fields, constructor `AuditEvent::new(kind, actor_name, action, outcome)`
//     pub actor_kind: ActorKind,          // SEC-40: actor type (default User)
//     pub project_id: Option<ProjectId>,  // OPS-12: project scoped log view, indexed (section 3.c)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActorKind { User, Token, Robot, Anonymous, System }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    pub id: Option<UserId>,
    pub name: String,
    pub kind: ActorKind,
}
impl ActorRef {
    pub fn system(name: impl Into<String>) -> Self;
    pub fn of(principal: &Principal) -> Self;
}
```

#### 1.3.6 `ports/auth.rs`

```rust
// core: crates/rustryx-core/src/ports/auth.rs  (additions)

// Authenticator: one provided method (gap 32). The chain overrides it; others inherit the default.
//     async fn authenticate_detailed(&self, credentials: &Credentials, context: &AuthContext)
//         -> Result<Option<Principal>, AuthFailure> {
//         self.authenticate(credentials, context).await.map_err(|error| AuthFailure { method: None, error })
//     }

// ResourceRef: #[non_exhaustive], two new fields; the constructor takes the repository so nothing can be forgotten.
//     pub allow_anonymous: bool,            // repositories.allow_anonymous (gap 13)
//     pub project: Option<ProjectRef>,      // repositories.project, for project roles (W2-B)
impl ResourceRef {
    /// The only constructor in production code: copies name, format, flag and project from the repository.
    pub fn for_repository(repository: &Repository, path: impl Into<String>) -> Self;
    #[must_use] pub fn with_coordinate(self, coordinate: Coordinate) -> Self;
}

// RbacChecker: provided methods, all fail closed.
#[async_trait]
pub trait RbacChecker: Send + Sync {
    // check, can_see_repository: unchanged.

    /// True when the principal holds `nx-admin` by role id and its token (if any) is unrestricted (gap 14).
    /// Default `Ok(false)`.
    async fn is_admin(&self, _principal: &Principal) -> Result<bool, AuthError> { Ok(false) }

    /// Project level permission (members, policies, robots, webhooks, ...). Default `Deny`, so an
    /// implementer that knows no projects refuses everything.
    async fn check_project(&self, _principal: &Principal, _project: ProjectId,
                           _permission: ProjectPermission) -> Result<Decision, AuthError> { Ok(Decision::Deny) }

    /// Application area privilege (`app.<area>.read` and `.write`, AUTH-46). `nx-admin` always passes.
    /// Default `Deny`.
    async fn check_app(&self, _principal: &Principal, _area: AppArea, _action: Action)
        -> Result<Decision, AuthError> { Ok(Decision::Deny) }

    /// True when the principal holds `action` on the WHOLE repository, with no selector restriction. This is the
    /// condition for handing that action to a robot token (cannot exceed the creator, AUTH-65). Default `Ok(false)`.
    async fn holds_whole_repository(&self, _principal: &Principal, _repository: &Repository,
                                    _action: Action) -> Result<bool, AuthError> { Ok(false) }

    /// Which projects the principal may see: all, or this explicit set plus every public project.
    /// Default is the empty set (see only public projects).
    async fn visible_projects(&self, _principal: &Principal) -> Result<ProjectVisibilityFilter, AuthError> {
        Ok(ProjectVisibilityFilter::PublicOnly)
    }
}

/// Answer of `visible_projects`, passed to `ProjectRepo::list_visible`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectVisibilityFilter {
    /// Administrators.
    All,
    /// Public projects only.
    PublicOnly,
    /// Public projects plus the projects in which the caller has a role (user id and external groups).
    Member { user_id: Option<UserId>, provider: Option<IdpProvider>, groups: Vec<String> },
}
```

Rbac side effects of section 1.3.6 (what `rustryx-rbac` implements) are in section 3.e. The decision order stays: anonymous write denied, token scopes, `nx-admin`, privileges, then (new) robot and project rules, then deny.

#### 1.3.7 `ports/blob.rs`

```rust
// core: crates/rustryx-core/src/ports/blob.rs  (additions)

// BlobMeta: #[non_exhaustive], doc fixed ("size and, when the store knows it, the modification time"), new field
//     pub modified: Option<OffsetDateTime>,
// BlobCommit: #[non_exhaustive], new field naming the member store that took the bytes (None for plain stores):
//     pub placement: Option<BlobStoreId>,

/// What a store can do, so that callers distinguish "none" from "cannot tell" (gap 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BlobStoreCapabilities {
    pub lists_staging: bool,
    pub lists_keys: bool,
    pub can_presign: bool,
    pub resumable: bool,
}

/// A page of keys for reconciliation (`verify_blobs`).
#[derive(Debug, Clone)]
pub struct KeyPage {
    pub keys: Vec<(BlobKey, BlobMeta)>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PresignOptions {
    pub response_content_type: Option<String>,
    /// Always `attachment; filename=...` for active content types (SEC-35).
    pub response_content_disposition: Option<String>,
}

pub struct PresignedUrl {
    /// A bearer capability. Never logged, never stored, never put in an audit row.
    pub url: Secret,
    pub expires_at: OffsetDateTime,
}

#[async_trait]
pub trait BlobStore: Send + Sync + 'static {
    // existing methods unchanged ...

    /// Capabilities. Default: none.
    fn capabilities(&self) -> BlobStoreCapabilities { BlobStoreCapabilities::default() }

    /// Releases background work (abort tasks). Default no-op (gap 8). Idempotent.
    async fn shutdown(&self) {}

    /// Lists committed keys after the opaque cursor `after`, at most `limit`, in a stable, store defined order (the local
    /// layout shards by a hash of the key, so key order is not promised and no caller may rely on it). Cancel safe.
    /// Default `Err(Unsupported("list_keys"))`.
    async fn list_keys(&self, _after: Option<&str>, _limit: u32) -> Result<KeyPage, BlobError> {
        Err(BlobError::Unsupported("list_keys"))
    }

    /// A time limited direct download URL, or `Ok(None)` when the store has none (local) or presigning is not
    /// enabled for it. Cancel safe. Default `Ok(None)`.
    async fn presign_get(&self, _key: &BlobKey, _ttl: Duration, _options: &PresignOptions)
        -> Result<Option<PresignedUrl>, BlobError> { Ok(None) }

    /// Contract for chunked uploads that span requests and nodes (gap 29). Default `None`.
    fn resumable(&self) -> Option<&dyn ResumableBlobStore> { None }
}

/// Chunked upload across requests. Each chunk is staged as its own object, so a chunk may be any size.
/// `finalize` streams the chunks in order through a normal writer, which hashes the content and commits
/// it. It reads every chunk once more: a SHA-256 state cannot be moved between nodes, and S3 parts under
/// 5 MiB are illegal, so composing server side is not an option. Monolithic uploads do not use this.
#[async_trait]
pub trait ResumableBlobStore: Send + Sync {
    /// New upload. The handle is opaque, URL safe, at most 128 bytes.
    async fn begin_resumable(&self, key: &BlobKey) -> Result<UploadHandle, BlobError>;
    /// Stages the chunk that starts at `offset`. An offset that is not the current length is
    /// `RangeNotSatisfiable` (the client resumes from the reported length). Not cancel safe for the handle:
    /// after a cancelled call the caller re-reads `length`.
    async fn append(&self, handle: &UploadHandle, offset: u64, chunk: Bytes) -> Result<(), BlobError>;
    /// Bytes staged so far.
    async fn length(&self, handle: &UploadHandle) -> Result<u64, BlobError>;
    /// Joins, hashes and commits. Abandoned handles are removed by the staging sweep after `gc.min_age_secs`.
    async fn finalize(&self, handle: UploadHandle) -> Result<BlobCommit, BlobError>;
    async fn abort(&self, handle: UploadHandle) -> Result<(), BlobError>;
}
```

#### 1.3.8 `ports/repos.rs`

```rust
// core: crates/rustryx-core/src/ports/repos.rs  (additions to existing traits)

// StoreError mapping rules (gap 3) go into the module doc.

// BlobRepo
//     async fn insert(&self, record: &BlobRecord) -> Result<(), StoreError>;                    // gap 2, required
//     async fn list_unreferenced_in(&self, store: BlobStoreId, older_than: OffsetDateTime, limit: u32)
//         -> Result<Vec<BlobRecord>, StoreError>;                  // required: GC takes one lock per store
//     /// GC sweep step. One transaction: lock the row `FOR UPDATE`, re-check that `orphaned_at` is older than
//     /// `older_than` and that no asset references the blob, delete the ROW and commit. `Ok(false)` when the blob
//     /// gained a reference or is too young (it is kept). The caller deletes the OBJECT only after `Ok(true)`: with
//     /// `AssetRepo::link` an orphan can be referenced again, and deleting the object first could leave an asset
//     /// pointing at nothing. This reverses the phase 1 wording of `delete` ("object first"); `delete` stays for
//     /// tests and tooling and is documented as unsafe for GC.
//     async fn delete_if_unreferenced(&self, id: BlobId, older_than: OffsetDateTime) -> Result<bool, StoreError>;
//     // used by W2-F: move_blob, list_page_by_store; signatures in section 7.b

// BlobStoreRepo
//     async fn update(&self, store: &BlobStoreDef) -> Result<(), StoreError>;                   // gap 10, required

// UserRepo
//     async fn set_password_hash_if(&self, id: UserId, old_hash: &str, new_hash: &str)
//         -> Result<bool, StoreError>;                             // gap 24, required, true when swapped

// AssetRepo: three required methods, two provided.
#[async_trait]
pub trait AssetRepo: Send + Sync {
    // existing: get_by_path, get, publish (becomes provided), list, list_by_component, delete_by_path, touch_downloaded

    /// The enforcing publish: write policy, immutability, hold, quota (before commit, on the actual size),
    /// read only mode and the outbox events all run in the same transaction as the asset switch. A refusal
    /// leaves nothing behind (no asset, no blob row, no outbox row). Cancel safe (one transaction).
    async fn publish_with(&self, new: &NewAsset, options: &WriteOptions) -> Result<PublishOutcome, MutationError>;

    /// Provided: `publish_with(new, &WriteOptions::new())` with `MutationError` flattened to `StoreError::Conflict`
    /// for violations. Kept so phase 1 code compiles; new code calls `publish_with`.
    async fn publish(&self, new: &NewAsset) -> Result<PublishOutcome, StoreError>;

    /// Publish by reference to an existing blob (same guards as `publish_with`). Usage is counted once per scope.
    async fn link(&self, link: &NewAssetLink, options: &WriteOptions) -> Result<PublishOutcome, MutationError>;

    /// Deletes with the same guards; returns the blob that lost its last reference.
    async fn delete_by_path_checked(&self, repository_id: RepositoryId, path: &str, options: &WriteOptions)
        -> Result<Option<BlobRecord>, MutationError>;

    /// Moves `updated_at` forward after an upstream revalidation (304), without touching the blob (gap 26).
    async fn touch_fresh(&self, id: AssetId, at: OffsetDateTime) -> Result<(), StoreError>;

    /// Batched downloads: sets `last_downloaded_at` (only when older than the configured interval) and adds
    /// the counts to the daily counters (W2-C). Idempotent per batch is NOT promised: a retry may double count.
    async fn record_downloads(&self, batch: &[DownloadTick]) -> Result<(), StoreError>;
}

// ComponentRepo: one required method, same guards.
//     async fn delete_checked(&self, id: ComponentId, options: &WriteOptions) -> Result<(), MutationError>;
// The old `delete` stays and is subject to the SQL guards (a violation surfaces as StoreError::Conflict).
```

The old state changing methods stay callable on purpose: the SQL guards (triggers `RX004` to `RX007`) apply to them too, so a forgotten typed call can never bypass immutability, holds or read only mode. The typed variants only add a precise error and events.

#### 1.3.9 `ports/infra.rs` and the shared system ports

```rust
// core: crates/rustryx-core/src/ports/infra.rs  (additions)

// InvalidationEvent: #[non_exhaustive]; two new variants. A node that does not know a payload decodes it as Reset
// (db.md section 9), so N-1 nodes stay correct.
//     /// Project, membership, group mapping, label scope or visibility changed.
//     Projects,
//     /// A settings section or a security policy changed (settings, gate, allowlist, trust policy); carries the
//     /// `system_config` revision so a node knows what it has seen.
//     Config(i64),

/// Persisted revocation of internal JWT ids (gap 21). Replaces the in-memory list of `rustryx-auth::internal_jwt`.
#[async_trait]
pub trait RevokedTokenRepo: Send + Sync {
    /// Idempotent. `expires_at` is the JWT `exp`, after which the row is useless and may be purged.
    async fn revoke(&self, tid: Uuid, expires_at: OffsetDateTime) -> Result<(), StoreError>;
    async fn is_revoked(&self, tid: Uuid) -> Result<bool, StoreError>;
    async fn purge_expired(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Durable queues with leases (gap 19). Exact semantics of `rustryx-db::PgJobLeases`; time comes from the database.
#[derive(Debug, Clone)]
pub struct JobLease {
    pub id: Uuid,
    pub queue: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub lease_expires_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub queue: String,
    pub dedupe_key: Option<String>,
    pub payload: serde_json::Value,
    pub delay: Duration,
}

#[async_trait]
pub trait JobLeaseRepo: Send + Sync {
    /// `true` when a row was inserted; a queued or running row with the same `(queue, dedupe_key)` makes it a no-op.
    async fn enqueue(&self, queue: &str, dedupe_key: Option<&str>, payload: &serde_json::Value)
        -> Result<bool, StoreError>;
    /// Like `enqueue`, available after `delay`.
    async fn enqueue_after(&self, queue: &str, dedupe_key: Option<&str>, payload: &serde_json::Value,
                           delay: Duration) -> Result<bool, StoreError>;
    async fn claim(&self, queue: &str, owner: &str, lease: Duration) -> Result<Option<JobLease>, StoreError>;
    /// `false` once the lease was lost: stop and write nothing more.
    async fn heartbeat(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StoreError>;
    async fn complete(&self, id: Uuid, owner: &str) -> Result<bool, StoreError>;
    async fn release_for_retry(&self, id: Uuid, owner: &str, retry_in: Duration, error: &str)
        -> Result<bool, StoreError>;
    /// Terminal failure (state `failed`), keeps `error` (at most 500 characters).
    async fn fail(&self, id: Uuid, owner: &str, error: &str) -> Result<bool, StoreError>;
    /// Inserts several jobs and, when `advance` is given, moves an outbox cursor in the same transaction (the
    /// atomic hand-off of section 2.2). Dedupe keys behave as in `enqueue`. Returns how many rows were inserted.
    async fn enqueue_batch(&self, jobs: &[NewJob], advance: Option<&CursorAdvance>) -> Result<u64, StoreError>;
    /// Queued plus running rows, for the queue depth gauge.
    async fn depth(&self, queue: &str) -> Result<u64, StoreError>;
    /// Removes `done` and `failed` rows older than `older_than`.
    async fn purge_finished(&self, older_than: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Read side of the audit log (NDJSON export, viewer, project log view). The sink stays write only.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AuditQuery {
    pub from: Option<OffsetDateTime>,
    pub to: Option<OffsetDateTime>,
    pub kinds: Vec<AuditKind>,
    pub actor_id: Option<UserId>,
    pub actor_name: Option<String>,
    pub action_prefix: Option<String>,
    pub target: Option<String>,
    pub project_id: Option<ProjectId>,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredAuditEvent { pub id: Uuid, pub event: AuditEvent }

#[async_trait]
pub trait AuditReadRepo: Send + Sync {
    /// Keyset paging newest first by `(event_time, id)`. A malformed token is `StoreError::Invalid`.
    async fn query(&self, query: &AuditQuery, page: &PageRequest) -> Result<Page<StoredAuditEvent>, StoreError>;
    /// Streams every match oldest first for NDJSON export; backpressured, cancel safe (a dropped stream ends the query).
    fn export(&self, query: &AuditQuery) -> BoxStream<'static, Result<StoredAuditEvent, StoreError>>;
}
```

```rust
// core: crates/rustryx-core/src/routing.rs  (new file; gaps 19, 27)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode { Allow, Block }

/// A routing rule (PRX-10). Patterns are regular expressions of the `regex` crate (linear time, no
/// backreferences) matched against the path AFTER the format's `classify` normalization. Checked before the
/// cache lookup, so a new block rule applies to cached content at once.
/// Limits at save time: at most 32 patterns, each at most 256 bytes, compiled size at most 256 KiB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoutingRule {
    pub id: RoutingRuleId,
    pub name: String,
    pub mode: RoutingMode,
    pub patterns: Vec<String>,
    pub description: String,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[async_trait]
pub trait RoutingRuleRepo: Send + Sync {
    async fn create(&self, rule: &RoutingRule) -> Result<(), StoreError>;
    async fn get(&self, id: RoutingRuleId) -> Result<Option<RoutingRule>, StoreError>;
    async fn list(&self) -> Result<Vec<RoutingRule>, StoreError>;
    async fn update(&self, rule: &RoutingRule, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a repository uses the rule; the repositories are in `assigned_to`.
    async fn delete(&self, id: RoutingRuleId) -> Result<(), StoreError>;
    async fn assigned_to(&self, id: RoutingRuleId) -> Result<Vec<RepositoryId>, StoreError>;
}
```

The shared system ports, which several streams use, are defined where they are first needed and are listed in the manifest: `OutboxRepo` and `EventKind` (section 2.2), `TaskDefinitionRepo` and `TaskRunRepo` (section 2.3), `NodeRegistryRepo` and `FeatureId` (section 2.4), `SystemConfigRepo` and the settings sections (section 8), `StatusCheckProvider` (section 8).

#### 1.3.10 Manifest of the stream owned core additions

The lead creates these files in the same commit. Each is a pure addition defined in the section named in the last column. `ports/mod.rs` gets one `pub mod` and one `pub use` line per row of the port files.

| New file in `rustryx-core/src` | Entities and ports | Defined in |
|---|---|---|
| `project.rs` | `ProjectName`, `ProjectVisibility`, `Project`, `ProjectRole`, `ProjectPermission`, `ProjectGrantSet`, `ProjectMember`, `ProjectSubject` | 3.a |
| `label.rs` | `LabelName`, `LabelScope`, `Label`, `ComponentLabel` | 3.a |
| `robot.rs` | `RobotToken`, `NewRobotToken`, `RobotScopeSet` | 3.a |
| `ports/projects.rs` | `ProjectRepo`, `ProjectMemberRepo`, `ProjectAccessRepo`, `LabelRepo`, `RobotTokenRepo` | 3.b |
| `quota.rs` | `QuotaScope`, `Quota`, `QuotaUsage`, `QuotaExceeded` | 4.a |
| `lifecycle.rs` | `PolicyScope`, `CleanupPolicy`, `CleanupCriteria`, `RetentionPolicy`, `RetentionRule`, `NameSelector`, `LifecycleRun`, `LifecycleHold`, `ImmutabilityRule`, `GcRun`, `SoftQuota` | 4.a |
| `ports/lifecycle.rs` | `QuotaRepo`, `ImmutabilityRepo`, `LifecycleHoldRepo`, `CleanupPolicyRepo`, `RetentionPolicyRepo`, `LifecycleRunRepo`, `LifecycleRepo`, `GcRunRepo` | 4.b |
| `events.rs` | `EventKind`, `EventDraft`, `OutboxEvent`, `OutboxCursor`, `OutboxBatch`, `CursorAdvance` | 2.2 |
| `tasks.rs` | `Schedule`, `TaskDefinition`, `TaskRun`, `TaskRunStatus`, `TaskTrigger`, `TaskNotify` | 2.3 |
| `node.rs` | `NodeInfo`, `FeatureId` | 2.4 |
| `scan.rs` | `Severity`, `ScanKind`, `ScanStatus`, `ScanReport`, `Finding`, `CveAllowlistEntry`, `GateMode`, `PullGatePolicy`, `ScanPolicy`, `GateDecision`, `GateReason`, `TrustPolicy`, `TrustMaterial`, `ComponentSecurityState` | 5.a |
| `ports/scans.rs` | `ScannerRepo`, `ScanRepo`, `CveAllowlistRepo`, `SecurityPolicyRepo`, `TrustRepo`, `SecurityHubRepo`, `ExportRepo`, `ScannerDataRepo` | 5.b |
| `registry.rs`, `replication.rs`, `webhook.rs`, `notification.rs`, `promotion.rs` | entities of W2-E | 6.a |
| `ports/distribution.rs` | `RegistryRepo`, `ReplicationRepo`, `WebhookRepo`, `WebhookDeliveryRepo`, `NotificationRepo`, `PromotionRepo` | 6.b |
| `storage.rs` | `FillPolicy`, `BlobGroupConfig`, `BlobStoreMember`, `BlobMove`, `VerifyRun`, `VerifyFinding`, `BlobStoreStats` | 7.a |
| `ports/storage.rs` | `BlobStoreMemberRepo`, `BlobMoveRepo`, `VerifyRunRepo`, `BlobStoreStatsRepo`, further additions to `BlobRepo` | 7.b |
| `settings.rs`, `status.rs` | settings sections, `TrustedCertificate`, `StatusCheck`, `StatusCheckProvider` | 8.a |
| `ports/system.rs` | `OutboxRepo`, `TaskDefinitionRepo`, `TaskRunRepo`, `NodeRegistryRepo`, `SystemConfigRepo`, `TrustedCertificateRepo` | 2 and 8.b |
| `routing.rs` | see 1.3.9 | 1.3.9 |

### 1.4 Breaking changes and the crates to adapt

| Change | Crates that must be adapted | What to do |
|---|---|---|
| `#[non_exhaustive]` on `StoreError`, `BlobError`, `AuthError`, `DomainError` and the new variants | `rustryx-api` (error mapping), `rustryx-auth` (matches on `AuthError`), `rustryx-format-core` (`From` conversions), `rustryx-db` (`error.rs`), `rustryx-testkit` | add a wildcard arm that maps to the 500 class; map the new variants as in section 1.3.1 |
| Struct literals of `Repository` (2 db files, 3 format-core, 1 rbac, 2 testkit files), `ProxyConfig` (2 db, 1 testkit), `BlobStoreDef` (2 blob, 2 db, 1 testkit), `Component`, `NewComponent`, `Asset`, `NewAsset` (db, format-core, testkit), `PublishOutcome` | `rustryx-db` (`rows.rs`, repos), `rustryx-format-core`, `rustryx-blob` (tests), `rustryx-testkit` (fixtures, store), `rustryx-rbac` (tests) | build with `::new(..)` and assign the optional fields; db rows set the new fields from the new columns |
| `Principal` literal (10 files in auth, 2 api, 2 format-core, 1 rbac, 1 testkit) | `rustryx-auth`, `rustryx-api`, `rustryx-format-core`, `rustryx-rbac`, `rustryx-testkit` | `Principal::new(..)` then assign |
| `Principal::is_admin()` removed | tests only: `rustryx-auth` (4 test files), `rustryx-rbac/tests/rbac_must_not.rs`, `rustryx-testkit/src/fixtures.rs`, core's own tests | replace by `has_role(ROLE_NX_ADMIN)` in tests; production code asks `Rbac::is_admin` |
| `AuthMethod`, `PrivilegeGrant`, `AuditKind`, `InvalidationEvent`, `BlobStoreKind`, `RepoFormat` non exhaustive; new variants | `rustryx-rbac` (matches on `PrivilegeGrant`), `rustryx-db` (text mapping of grants, audit kinds, store kinds, bus payloads), `rustryx-auth`, `rustryx-testkit` (bus), `rustryx-format-core` (registry iterates `RepoFormat::ALL`) | wildcard arms; `ALL` is now a slice |
| `ResourceRef` literal (api 1, format-core 2, rbac 2) and `Coordinate` literal (format-core 2, rbac 5) | `rustryx-api`, `rustryx-format-core`, `rustryx-rbac` | `ResourceRef::for_repository(repo, path)`; `Coordinate::new` |
| `ExternalGroups` literal (3 auth files), `AuthContext` literal (api 1, auth 2) | `rustryx-auth` (ldap, oidc, chain), `rustryx-api` | constructors; the LDAP resolver sets `confirmed_at` to the directory answer time |
| `AuditEvent` literal (db, format-core, testkit) | `rustryx-db`, `rustryx-format-core`, `rustryx-testkit` | `AuditEvent::new`; db stores `actor_kind` and `project_id` (migration 0007 adds the columns) |
| `BlobMeta`, `BlobCommit` literals (blob 2 and 4 files, format-core 1, testkit) | `rustryx-blob`, `rustryx-format-core`, `rustryx-testkit` | constructors with `modified: None`, `placement: None` |
| Required methods added: `BlobRepo::insert`, `list_unreferenced_in`, `delete_if_unreferenced`, `BlobStoreRepo::update`, `UserRepo::set_password_hash_if`, `AssetRepo::publish_with`, `link`, `delete_by_path_checked`, `touch_fresh`, `record_downloads`, `ComponentRepo::delete_checked` | implementers: `rustryx-db`, `rustryx-testkit` | implement; `publish` becomes the provided wrapper |
| `BlobStoreRegistry` (trait in format-core) gains required `store_by_id` and provided `store_for_blob(&BlobRecord)` (section 7.b) | `rustryx-format-core` (download path), `rustryx-blob` (adapter) | download uses `store_for_blob` |

Everything else in this document is additive: new files, new tables, new provided methods, new variants behind `#[non_exhaustive]`.

### 1.5 Foundation migration `0007_wave2_foundation.sql`

Applied with Core revision 2 (the db owner writes it from this list). It contains only what more than one stream references, so the streams can start in parallel with their foreign keys in place. All of it is expand only (section 2.4); nothing here is read by an N-1 binary, and the one trigger that touches an existing table (`repositories`) is safe for N-1 writers.

```sql
-- projects: the container (full table, W2-B owns later additions)
CREATE TABLE projects (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name ~ '^[a-z0-9]+([._-][a-z0-9]+)*$' AND length(name) BETWEEN 2 AND 64),
    description text        NOT NULL DEFAULT '' CHECK (length(description) <= 4000),
    visibility  text        NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_projects_name UNIQUE (name)
);

ALTER TABLE repositories ADD COLUMN project_id uuid REFERENCES projects (id) ON DELETE RESTRICT;
CREATE INDEX ix_repositories_project ON repositories (project_id) WHERE project_id IS NOT NULL;

-- Invariant: allow_anonymous of a repository in a project follows the project visibility. An N-1 binary that
-- updates a repository row and writes its old allow_anonymous value is overruled by this trigger.
CREATE FUNCTION repositories_project_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.project_id IS NOT NULL THEN
        NEW.allow_anonymous := (SELECT visibility = 'public' FROM projects WHERE id = NEW.project_id);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER repositories_project_sync BEFORE INSERT OR UPDATE OF project_id, allow_anonymous ON repositories
    FOR EACH ROW EXECUTE FUNCTION repositories_project_sync();

CREATE FUNCTION projects_visibility_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE repositories SET allow_anonymous = (NEW.visibility = 'public') WHERE project_id = NEW.id;
    PERFORM pg_notify('rustryx_rbac', 'projects');
    RETURN NULL;
END $$;
CREATE TRIGGER projects_visibility_sync AFTER UPDATE OF visibility ON projects
    FOR EACH ROW EXECUTE FUNCTION projects_visibility_sync();

-- uploader, pull time, audit scope
ALTER TABLE components ADD COLUMN created_by uuid REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE components ADD COLUMN last_downloaded_at timestamptz;
ALTER TABLE assets     ADD COLUMN created_by uuid REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE audit_events ADD COLUMN actor_kind text NOT NULL DEFAULT 'user';
ALTER TABLE audit_events ADD COLUMN project_id uuid;
CREATE INDEX ix_audit_events_project ON audit_events (project_id, event_time DESC) WHERE project_id IS NOT NULL;

-- settings revision and author (section 8). The revision comes from one sequence, so max(revision) is a monotonic
-- high water mark that nodes compare ("which settings have you applied").
CREATE SEQUENCE system_config_revision_seq;
ALTER TABLE system_config ADD COLUMN revision   bigint NOT NULL DEFAULT nextval('system_config_revision_seq');
ALTER TABLE system_config ADD COLUMN updated_by uuid;
CREATE FUNCTION system_config_bump() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.revision := nextval('system_config_revision_seq'); NEW.updated_at := now(); RETURN NEW; END $$;
CREATE TRIGGER system_config_bump BEFORE UPDATE ON system_config FOR EACH ROW EXECUTE FUNCTION system_config_bump();

-- revoked internal JWTs (gap 21)
CREATE TABLE revoked_tokens (tid uuid PRIMARY KEY, expires_at timestamptz NOT NULL);
CREATE INDEX ix_revoked_tokens_expiry ON revoked_tokens (expires_at);

-- node registry and feature levels (section 2.4)
CREATE TABLE node_heartbeats (
    node_id           text PRIMARY KEY CHECK (node_id <> '' AND length(node_id) <= 200),
    version           text        NOT NULL,
    feature_level     integer     NOT NULL CHECK (feature_level >= 1),
    started_at        timestamptz NOT NULL,
    seen_at           timestamptz NOT NULL,
    settings_revision bigint      NOT NULL DEFAULT 0,
    inflight_writes   integer     NOT NULL DEFAULT 0,
    details           jsonb       NOT NULL DEFAULT '{}'::jsonb
);

-- event outbox and cursors (section 2.2)
CREATE SEQUENCE event_outbox_seq;
CREATE TABLE event_outbox (
    seq           bigint      NOT NULL DEFAULT nextval('event_outbox_seq'),
    tx_id         xid8        NOT NULL DEFAULT pg_current_xact_id(),
    id            uuid        NOT NULL,
    occurred_at   timestamptz NOT NULL DEFAULT now(),
    kind          text        NOT NULL,
    project_id    uuid,
    repository_id uuid,
    actor_id      uuid,
    actor_name    text        NOT NULL,
    actor_kind    text        NOT NULL,
    request_id    text,
    data          jsonb       NOT NULL,
    PRIMARY KEY (occurred_at, seq)
) PARTITION BY RANGE (occurred_at);
CREATE INDEX ix_event_outbox_relay ON event_outbox (tx_id, seq);
CREATE UNIQUE INDEX uq_event_outbox_id ON event_outbox (id, occurred_at);
CREATE TABLE outbox_cursors (
    consumer   text PRIMARY KEY,
    last_tx_id xid8   NOT NULL DEFAULT '0',
    last_seq   bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- plus ensure_outbox_partitions(now, lookahead) and drop_old_outbox_partitions(retention_days), written like the
-- audit functions of 0005 (generated names only, advisory transaction lock).

-- task framework (section 2.3)
CREATE TABLE task_definitions (
    id          uuid PRIMARY KEY,
    kind        text        NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    params      jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(params) = 'object'),
    schedule    jsonb       NOT NULL CHECK (jsonb_typeof(schedule) = 'object'),
    enabled     boolean     NOT NULL DEFAULT true,
    system      boolean     NOT NULL DEFAULT false,
    notify      jsonb       NOT NULL DEFAULT '{"on_failure": false, "on_success": false}'::jsonb,
    next_run_at timestamptz,
    last_run_id uuid,
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_task_definitions_name UNIQUE (name)
);
CREATE INDEX ix_task_definitions_due ON task_definitions (next_run_at) WHERE enabled AND next_run_at IS NOT NULL;

CREATE TABLE task_runs (
    id                uuid PRIMARY KEY,
    task_id           uuid        REFERENCES task_definitions (id) ON DELETE SET NULL,
    kind              text        NOT NULL,
    scope_key         text        NOT NULL DEFAULT '',
    status            text        NOT NULL CHECK (status IN ('queued','running','succeeded','failed','cancelled','skipped','abandoned')),
    trigger           text        NOT NULL CHECK (trigger IN ('schedule','manual','event','startup')),
    dry_run           boolean     NOT NULL DEFAULT false,
    params            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    node_id           text,
    lease_expires_at  timestamptz,
    heartbeat_at      timestamptz,
    stop_requested_at timestamptz,
    queued_at         timestamptz NOT NULL DEFAULT now(),
    started_at        timestamptz,
    finished_at       timestamptz,
    summary           jsonb       NOT NULL DEFAULT '{}'::jsonb,
    error             text        CHECK (error IS NULL OR length(error) <= 500),
    attempts          integer     NOT NULL DEFAULT 0
);
-- One queued or running run per (kind, scope): the database side of "singleton task".
CREATE UNIQUE INDEX uq_task_runs_active ON task_runs (kind, scope_key) WHERE status IN ('queued', 'running');
CREATE INDEX ix_task_runs_task ON task_runs (task_id, queued_at DESC);
CREATE INDEX ix_task_runs_claim ON task_runs (queued_at) WHERE status = 'queued';

CREATE TABLE task_run_log (
    run_id  uuid        NOT NULL REFERENCES task_runs (id) ON DELETE CASCADE,
    seq     integer     NOT NULL,
    at      timestamptz NOT NULL DEFAULT now(),
    level   smallint    NOT NULL,
    message text        NOT NULL CHECK (length(message) <= 1024),
    PRIMARY KEY (run_id, seq)
);
```

Not in 0007 on purpose: `routing_rules` and `cleanup_policies` (their owners add them in their own range, W2-A and W2-C), everything stream specific.

### 1.6 Order of application for the lead

1. Apply sections 1.3.1 to 1.3.9 and the manifest files (1.3.10) to `rustryx-core`; `cargo check -p rustryx-core`.
2. Adapt the crates of section 1.4 in the order `rustryx-testkit`, `rustryx-db` (with 0007), `rustryx-blob`, `rustryx-rbac`, `rustryx-auth`, `rustryx-format-core`, `rustryx-api`, `rustryx-server`. Each is a mechanical change, except `rustryx-db` (new implementations of the required methods) and `rustryx-testkit` (same, in memory).
3. Add the config keys of section 2.5 (`egress.*`, `tasks.*`, `outbox.*`) and `auth.oidc.authorized_parties` (gap 17) to `rustryx-config` with its docs bless step.
4. Create the stub crates `rustryx-egress` and the new crates of section 2.5 (empty `lib.rs`, correct `Cargo.toml`, workspace member) so every stream owner has a place to work.
5. Start the streams in the order of section 12.

---

## 2. Cross-cutting mechanisms

### 2.1 Secrets at rest

Three classes, one rule each.

| Class | Examples | Storage | Returned by the API |
|---|---|---|---|
| Verified only | API and robot tokens, scan access tokens | lowercase hex SHA-256 of the whole token (256 bit secret, so no salt or work factor helps) | never; shown once at creation or rotation |
| Used to authenticate outwards | webhook secret and auth header, registry password, SMTP password, outbound proxy password, scanner credential | sealed envelope `rx1.<key id>.<base64url>` (AES-256-GCM, db.md section 7) | `SecretStatus { set }` only |
| Not secret, but sensitive | certificates, public keys, trust policies | plain text | yes, except private key material, which is refused at upload |

Sealing is done by the repository implementation in `rustryx-db`, as `PgRepositoryRepo` does for upstream passwords today. The associated data is `purpose || ':' || row id`, so an envelope copied to another row or column does not open. The purposes are fixed strings: `webhook.secret`, `webhook.auth_header`, `registry.credential`, `smtp.password`, `proxy.credential`, `scanner.credential`, `blob_store.credential` (only when a store definition ever carries one; today secret references are used). `reseal_*` jobs for the new purposes follow `reseal_upstream_secrets` (compare and set per row), so a retired key can be dropped.

Every response type that has a secret has two tests: a type level one (no `Secret` field in any DTO, gap 34) and a behavioural one that creates the entity with a canary secret and searches every JSON response, `Debug` output and log line of the test run for the canary.

### 2.2 Event outbox

Purpose: one durable, ordered log of "something happened" that webhooks, e-mail, replication, scan on push and any later consumer read, written in the same transaction as the state change, so an event exists if and only if the change committed.

```rust
// core: crates/rustryx-core/src/events.rs  (new file)
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::audit::ActorRef;
use crate::ids::{EventId, ProjectId, RepositoryId};

/// The event catalog. Wire names are dotted because the specification fixes them (DSO-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    #[serde(rename = "push")] Push,
    #[serde(rename = "pull")] Pull,
    #[serde(rename = "delete")] Delete,
    #[serde(rename = "scan.completed")] ScanCompleted,
    #[serde(rename = "scan.failed")] ScanFailed,
    #[serde(rename = "scan.stopped")] ScanStopped,
    #[serde(rename = "pull.blocked")] PullBlocked,
    #[serde(rename = "quarantine.entered")] QuarantineEntered,
    #[serde(rename = "quarantine.released")] QuarantineReleased,
    #[serde(rename = "quota.exceeded")] QuotaExceeded,
    #[serde(rename = "quota.near_threshold")] QuotaNearThreshold,
    #[serde(rename = "replication.finished")] ReplicationFinished,
    #[serde(rename = "retention.finished")] RetentionFinished,
    #[serde(rename = "gc.finished")] GcFinished,
    #[serde(rename = "promotion.requested")] PromotionRequested,
    #[serde(rename = "promotion.approved")] PromotionApproved,
    #[serde(rename = "promotion.rejected")] PromotionRejected,
    #[serde(rename = "promotion.completed")] PromotionCompleted,
    /// Internal, feeds e-mail notifications only.
    #[serde(rename = "task.finished")] TaskFinished,
    /// Internal, feeds e-mail notifications only.
    #[serde(rename = "token.expiring")] TokenExpiring,
}

impl EventKind {
    #[must_use] pub const fn as_str(self) -> &'static str;
    #[must_use] pub fn parse(label: &str) -> Option<Self>;
    /// Everything except the two internal kinds. A webhook can only subscribe to these.
    #[must_use] pub const fn webhook_subscribable(self) -> bool;
    /// `Pull` only: written to the outbox only while some enabled webhook subscribes to it.
    #[must_use] pub const fn high_volume(self) -> bool;
}

/// An event as a producer describes it. The database assigns id, sequence and time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EventDraft {
    pub kind: EventKind,
    pub project_id: Option<ProjectId>,
    pub repository_id: Option<RepositoryId>,
    pub actor: ActorRef,
    pub request_id: Option<String>,
    /// Payload `data`. No secrets, no client addresses, at most 16 KiB (a larger value is `StoreError::Invalid`).
    pub data: serde_json::Value,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OutboxEvent {
    pub id: EventId,
    pub seq: i64,
    pub occurred_at: OffsetDateTime,
    pub kind: EventKind,
    pub project_id: Option<ProjectId>,
    pub repository_id: Option<RepositoryId>,
    pub actor: ActorRef,
    pub request_id: Option<String>,
    pub data: serde_json::Value,
}

/// Read position of one consumer: the transaction id and sequence of the last event it handled.
/// Opaque to callers (`tx_id` is the `xid8` of the writing transaction as `u64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutboxCursor { pub tx_id: u64, pub seq: i64 }

#[derive(Debug, Clone)]
pub struct OutboxBatch {
    pub events: Vec<OutboxEvent>,
    pub from: OutboxCursor,
    pub to: OutboxCursor,
}

/// Moves a consumer's cursor from `from` to `to` inside the transaction of the consuming write.
#[derive(Debug, Clone)]
pub struct CursorAdvance { pub consumer: &'static str, pub from: OutboxCursor, pub to: OutboxCursor }
```

```rust
// core: crates/rustryx-core/src/ports/system.rs  (new file, part 1)
#[async_trait]
pub trait OutboxRepo: Send + Sync {
    /// Writes events in their own transaction. For events that have no state change to ride on (pull, a rejection).
    async fn enqueue(&self, events: &[EventDraft]) -> Result<(), StoreError>;
    /// Events after the consumer's cursor whose writing transaction has finished (see the horizon rule below), in
    /// `(tx_id, seq)` order, at most `limit`. An empty batch carries `from == to`. Cancel safe, read only.
    async fn read_batch(&self, consumer: &str, limit: u32) -> Result<OutboxBatch, StoreError>;
    /// Moves the cursor with compare and set on `from`. `false` means another instance moved it first: the caller drops its batch.
    async fn advance(&self, advance: &CursorAdvance) -> Result<bool, StoreError>;
    /// Age of the oldest event the consumer has not handled, zero when there is none. Feeds `outbox_lag_seconds`.
    async fn lag(&self, consumer: &str) -> Result<Duration, StoreError>;
    /// Drops whole partitions older than `older_than` and returns how many. Never drops past the slowest cursor.
    async fn purge(&self, older_than: OffsetDateTime) -> Result<u64, StoreError>;
}
```

How the relay stays correct:

1. **Same transaction.** A state changing port method that must emit takes `WriteOptions.events` and inserts the rows in its own transaction (publish, delete, lifecycle apply, promotion, quota threshold events computed by the db). A rejected publish writes no event because it rolls back; the rejection itself (`quota.exceeded`, `pull.blocked`) is emitted by the caller with `OutboxRepo::enqueue`, which is allowed to be non atomic because no state changed.
2. **No lost event by commit reordering.** A sequence value is allocated at insert, but commit order can differ from sequence order, so a plain `seq > cursor` read can skip a row that commits late. `tx_id` is the `xid8` of the inserting transaction and a batch is read with `WHERE (tx_id, seq) > (cursor) AND tx_id < pg_snapshot_xmin(pg_current_snapshot())`. Rows below that horizon belong to finished transactions, so the set of rows below the horizon never gains a smaller key. A long open transaction holds the horizon back: the db sets `idle_in_transaction_session_timeout` to 60 s for the application role and `RustryxOutboxLag` fires on lag above 5 minutes.
3. **Atomic hand-off.** A consumer is a task of kind `outbox_consumer` under the lock `outbox:<consumer>`. It reads a batch, decides in Rust, and writes its own rows and the cursor move in one transaction through a repository method that takes `Option<&CursorAdvance>`: `WebhookDeliveryRepo::create_for_events`, `NotificationRepo::create_for_events`, `ReplicationRepo::create_event_tasks`, `JobLeaseRepo::enqueue_batch` (scan queue), and `OutboxRepo::advance` alone for a batch with nothing to write. A lost lock (`LockLost`) or a duplicate consumer makes the compare and set fail, so the batch is dropped and read again. Delivery to the outside world is at least once from there on; the stable event id and delivery id let a receiver deduplicate.
4. **Consumers.** `webhooks`, `email`, `replication`, `scan`. A new consumer is a new cursor name and no schema change. Each polls every `outbox.poll_interval_ms` (default 1000) and also wakes on `NOTIFY rustryx_outbox` (sent by the enqueue path, payload empty), so latency is normally below a second.
5. **Retention.** Monthly partitions of `event_outbox` by `occurred_at`; `outbox_purge` (daily) drops partitions that lie wholly before `now - outbox.retention_days` (default 30, so between 30 and 61 days are kept) and never past the slowest cursor. The outbox is not the audit log: audit rows are written by `AuditSink` directly (they must exist for failures and for events that are not in the catalog) and are never derived from the outbox.
6. **Push granularity.** One `push` event per component, not per asset: the format handler marks the upload that completes a component (OCI: the manifest `PUT`; Maven: the primary artifact) and format-core attaches the event to that publish only.
7. **Pull volume.** `Pull` events are not written unless an enabled webhook subscribes to them. The flag is derived from `webhooks` and cached 60 s. They go through `OutboxRepo::enqueue` from a bounded in-memory buffer flushed every second (capacity 10 000, overflow dropped and counted in `outbox_events_dropped_total{kind}`), never from the request path.

### 2.3 Task framework and distributed locks

The scheduler, runner and run history live in `rustryx-tasks`. Core holds the data types and the two repository ports, so any crate can enqueue or read runs.

```rust
// core: crates/rustryx-core/src/tasks.rs  (new file)
/// UTC only. Hourly, daily, weekly and monthly are cron shortcuts of the UI; there is no time zone, so no DST ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    Manual,
    Once { at: OffsetDateTime },
    Every { secs: u32 },
    /// Five field cron, validated by `rustryx-tasks` (the parser is not a dependency of core).
    Cron { expr: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskNotify { pub on_failure: bool, pub on_success: bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskDefinition {
    pub id: TaskDefinitionId,
    /// Key of a registered `TaskHandler` (fixed vocabulary, see the job catalog).
    pub kind: String,
    pub name: String,
    pub params: serde_json::Value,
    pub schedule: Schedule,
    pub enabled: bool,
    /// System tasks are seeded, cannot be deleted or have their kind or params changed; schedule, enabled and notify are editable.
    pub system: bool,
    pub notify: TaskNotify,
    pub next_run_at: Option<OffsetDateTime>,
    pub last_run_id: Option<TaskRunId>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskRunStatus { Queued, Running, Succeeded, Failed, Cancelled, Skipped, Abandoned }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskTrigger { Schedule, Manual, Event, Startup }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskRun {
    pub id: TaskRunId,
    pub task_id: Option<TaskDefinitionId>,
    pub kind: String,
    /// Part of the singleton key: a store id, a policy id, a rule id, or empty.
    pub scope_key: String,
    pub status: TaskRunStatus,
    pub trigger: TaskTrigger,
    pub dry_run: bool,
    pub params: serde_json::Value,
    pub node_id: Option<String>,
    pub queued_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// Counts and a short text, never secrets, at most 8 KiB.
    pub summary: serde_json::Value,
    pub error: Option<String>,
    pub stop_requested: bool,
}

pub struct NewTaskRun { pub task_id: Option<TaskDefinitionId>, pub kind: String, pub scope_key: String,
                        pub trigger: TaskTrigger, pub dry_run: bool, pub params: serde_json::Value }
pub struct TaskLogLine { pub seq: i32, pub at: OffsetDateTime, pub level: u8, pub message: String }
pub struct TaskOutcome { pub status: TaskRunStatus, pub summary: serde_json::Value, pub error: Option<String> }
pub struct Heartbeat { pub owned: bool, pub stop_requested: bool }
pub struct Reaped { pub requeued: u64, pub abandoned: u64 }
```

```rust
// core: crates/rustryx-core/src/ports/system.rs  (part 2)
#[async_trait]
pub trait TaskDefinitionRepo: Send + Sync {
    async fn create(&self, definition: &TaskDefinition) -> Result<(), StoreError>;
    async fn get(&self, id: TaskDefinitionId) -> Result<Option<TaskDefinition>, StoreError>;
    async fn list(&self, kind: Option<&str>) -> Result<Vec<TaskDefinition>, StoreError>;
    /// Replaces schedule, enabled, notify and (for non system tasks) name and params. `Conflict` on a stale version.
    async fn update(&self, definition: &TaskDefinition, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` for a system task.
    async fn delete(&self, id: TaskDefinitionId) -> Result<(), StoreError>;
    /// Inserts the missing system definitions; existing rows (edited schedules) are left alone. Idempotent.
    async fn seed(&self, definitions: &[TaskDefinition]) -> Result<u64, StoreError>;
    /// Enabled definitions with `next_run_at <= now`, oldest first.
    async fn due(&self, now: OffsetDateTime, limit: u32) -> Result<Vec<TaskDefinition>, StoreError>;
    /// Compare and set of `next_run_at`: exactly one node wins a due time, so a schedule fires once without a lock.
    async fn advance(&self, id: TaskDefinitionId, expected: Option<OffsetDateTime>, next: Option<OffsetDateTime>)
        -> Result<bool, StoreError>;
}

#[async_trait]
pub trait TaskRunRepo: Send + Sync {
    /// `None` when a queued or running run of the same `(kind, scope_key)` exists (the unique index).
    async fn enqueue(&self, run: &NewTaskRun) -> Result<Option<TaskRun>, StoreError>;
    /// Claims the oldest queued run of one of `kinds` (`SKIP LOCKED`), sets lease and node.
    async fn claim(&self, node_id: &str, kinds: &[&str], lease: Duration) -> Result<Option<TaskRun>, StoreError>;
    /// Puts a claimed run back (the singleton lock was not free), available again after `delay`.
    async fn release(&self, id: TaskRunId, node_id: &str, delay: Duration) -> Result<bool, StoreError>;
    async fn heartbeat(&self, id: TaskRunId, node_id: &str, lease: Duration) -> Result<Heartbeat, StoreError>;
    /// Terminal state. Also writes the `task.finished` outbox event when the definition asks for it, in the same transaction.
    async fn finish(&self, id: TaskRunId, node_id: &str, outcome: &TaskOutcome) -> Result<bool, StoreError>;
    /// Sets `stop_requested_at`; the runner sees it at the next heartbeat. `false` for a finished run.
    async fn request_stop(&self, id: TaskRunId) -> Result<bool, StoreError>;
    async fn get(&self, id: TaskRunId) -> Result<Option<TaskRun>, StoreError>;
    async fn list(&self, task: Option<TaskDefinitionId>, page: &PageRequest) -> Result<Page<TaskRun>, StoreError>;
    /// Appends lines. The per run cap is 5000 lines; later lines are dropped and one truncation line is kept.
    async fn append_log(&self, id: TaskRunId, lines: &[TaskLogLine]) -> Result<(), StoreError>;
    async fn read_log(&self, id: TaskRunId, after_seq: i32, limit: u32) -> Result<Vec<TaskLogLine>, StoreError>;
    /// Runs whose lease expired: resumable kinds go back to `queued` (at most 3 attempts), others become `abandoned`.
    async fn reap_expired(&self, now: OffsetDateTime, resumable_kinds: &[&str]) -> Result<Reaped, StoreError>;
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}
```

```rust
// crate: rustryx-tasks  (first deliverable of W2-C, "C0", published before D, E, F and H start)
#[async_trait]
pub trait TaskHandler: Send + Sync + 'static {
    /// Key of the job catalog; also the `job` label of the metrics.
    fn kind(&self) -> &'static str;
    fn spec(&self) -> TaskSpec;
    /// Parameter validation, called when a definition is saved and again before every run.
    fn validate(&self, params: &serde_json::Value) -> Result<(), TaskError>;
    /// Runs until done or cancelled. Cancellation is cooperative: check `cx.cancelled()` between batches. A lost
    /// lock cancels the same token, after which the handler must write nothing more.
    async fn run(&self, cx: &mut TaskContext, params: &serde_json::Value) -> Result<TaskSummary, TaskError>;
}

pub struct TaskSpec {
    /// `None`: not a singleton. `Some(f)`: the lock is `task:<kind>` plus `:<f(params)>` when `f` returns a scope.
    pub singleton: Option<fn(&serde_json::Value) -> Option<String>>,
    /// May the runner start it again after a crash? (`reap_expired`).
    pub resumable: bool,
    /// Skipped while read only mode is on (section 4).
    pub mutates_content: bool,
    pub supports_dry_run: bool,
    pub default_schedule: Option<Schedule>,
    /// Staleness threshold for the `RustryxJobStale` alert, seconds.
    pub stale_after_secs: Option<u64>,
}
```

Runner behaviour, once, for every kind:

1. **Scheduling needs no lock.** Every node runs a tick every `tasks.scheduler_tick_secs` (default 15, with jitter). For each due definition it computes the next time, wins the compare and set `advance`, and only the winner enqueues the run. If the unique index refuses the run (one is still active) the tick counts `background_job_runs_total{result="skipped"}`.
2. **Execution under a lock.** Every node claims queued runs of the kinds it has handlers for, at most `tasks.max_concurrent_runs` at a time (default 4: a held PostgreSQL lock costs one dedicated direct connection, db.md section 8). For a singleton kind it takes `LockBackend::try_acquire("task:<kind>[:<scope>]", lock.ttl_secs)`; the effective advisory key preimage is `rustryx:lock:task:<kind>[:<scope>]`, matching 01-architecture. A lock that is not free puts the run back with a 30 s delay. A guard renewed every TTL/3 that reports `is_lost()` cancels the run's token and finishes it `failed` with `lock_lost`, and `distlock_lost_total` counts it. No lock is taken when the backend is unavailable (`Err`): the run stays queued, which is the README 8.2 rule (requests are served, singleton tasks do not start).
3. **Lease and stop.** The runner heartbeats the run row at the same cadence (`heartbeat_at`, `lease_expires_at`, `node_id`, SRE-23). A heartbeat that returns `stop_requested` cancels the token, so "stop" takes effect within one TTL/3 (about 10 s).
4. **Orphans.** `task_reaper` (every 30 s, lock `task_reaper`) re-queues or abandons runs whose lease expired.
5. **Read only mode.** A kind with `mutates_content` that starts while read only mode is on finishes `skipped` with reason `read_only`.
6. **Metrics.** `background_job_runs_total{job,result}` with `result` in `ok`, `error`, `skipped`; `background_job_running{job}`; `background_job_duration_seconds{job}`; `background_job_last_success_timestamp_seconds{job}` initialised to process start (slo.md A3).

**Job catalog.** One vocabulary for the `job` label, the lock key, the task kind and the staleness table of slo.md (assumption A8 grows by every row with a threshold). "Node" means a daemon on every node without a lock (not a task definition).

| `job` / kind | Stream | Execution | Default schedule (UTC) | Lock key | Mutates content | Alert if no success for |
|---|---|---|---|---|---|---|
| `blob_gc` | C | task per blob store | hourly (`gc.interval_secs`) | `task:blob_gc:<store>` | yes | 2 days |
| `cleanup` | C | task, one run per repository with attached policies | daily 01:00 | `task:cleanup:<repository>` | yes | 2 days |
| `retention` | C | task per retention policy | daily 02:00 | `task:retention:<policy>` | yes | 2 days |
| `cleanup_labels` | C | task, user created | weekly Sunday 03:00 | `task:cleanup_labels:<definition>` | yes | none |
| `download_flush` | C | node daemon, every 10 s | n/a | none | no | n/a (gauge `download_counter_pending`) |
| `download_partitions` | C | task | daily 02:10 | `task:download_partitions` | no | 3 days |
| `quota_recount` | C | task | weekly Sunday 04:00 and on demand | `task:quota_recount` | no | 9 days |
| `blob_store_stats` | C | task | every 5 min | `task:blob_store_stats` | no | 1 hour |
| `task_reaper` | C | framework | every 30 s | `task:task_reaper` | no | 10 min |
| `task_purge` | C | task | daily 03:10 | `task:task_purge` | no | 3 days |
| `node_heartbeat` | C | node daemon, every 30 s | n/a | none | no | n/a (gauge `node_heartbeat_age_seconds`) |
| `audit_rotation` | existing | task | daily 00:30 | `task:audit_rotation` | no | 2 days |
| `login_attempts_purge` | existing | task | hourly | `task:login_attempts_purge` | no | 1 day |
| `outbox_consumer` | E | task per consumer, runs until cancelled (`resumable`); the 60 s schedule only re-enqueues a missing run | every 60 s | `outbox:<consumer>` | no | 10 min |
| `outbox_purge` | E | task | daily 02:40 | `task:outbox_purge` | no | 3 days |
| `webhook_delivery` | E | node daemon (claims rows) | n/a | none | no | n/a (backlog gauge) |
| `webhook_purge` | E | task | daily 02:50 | `task:webhook_purge` | no | 3 days |
| `email_delivery` | E | node daemon | n/a | none | no | n/a |
| `replication` | E | task per rule (schedule), event consumer for event rules | per rule | none, DB uniqueness per rule | yes | per rule |
| `replication_worker` | E | node daemon (claims tasks) | n/a | none | yes | n/a |
| `registry_probe` | E | task | every 15 min | `task:registry_probe` | no | 1 hour |
| `promotion_execute` | E | job lease queue `promotion` | on demand | none (queue lease) | yes | none |
| `scan_worker` | D | node daemon (claims queue `scan`) | n/a | none | no | n/a (gauge `scan_queue_depth`) |
| `scan_all` | D | task | none by default | `task:scan_all` | no | none |
| `scan_rescan_stale` | D | task | daily 05:00 | `task:scan_rescan_stale` | no | 2 days |
| `scanner_dataset_update` | D | task per data set | every 6 h | `task:scanner_dataset_update:<dataset>` | no | 2 days |
| `scanner_probe` | D | node daemon, every 5 min | n/a | none | no | n/a (gauge `scanner_up`) |
| `hub_stats` | D | task | every 5 min | `task:hub_stats` | no | 1 hour |
| `export_purge` | D | task | hourly | `task:export_purge` | no | 1 day |
| `allowlist_expiry` | D | task | daily 00:20 | `task:allowlist_expiry` | no | 2 days |
| `verify_blobs` | F | task per blob store | weekly Sunday 05:00 (report mode) | `task:verify_blobs:<store>` | no (report), yes (repair) | 9 days |
| `blob_member_move` | F | task, manual | none | `task:blob_member_move:<group>` | yes | none |
| `blob_move_cleanup` | F | task | every 10 min | `task:blob_move_cleanup` | yes | 1 hour |
| `robot_token_purge` | B | task | daily 03:30 | `task:robot_token_purge` | no | 3 days |
| `token_expiry_notice` | B | task | daily 06:00 | `task:token_expiry_notice` | no | 3 days |
| `settings_refresh` | H | node daemon, every 5 s | n/a | none | no | n/a (gauge `settings_revision_applied`) |
| `cert_expiry_check` | H | task | daily 06:30 | `task:cert_expiry_check` | no | 3 days |

All tasks are idempotent (SRE-21). Progress is in PostgreSQL (the run row, the lifecycle run, the queue rows), never only in memory.

### 2.4 Feature levels and N-1 safety for wave 2

README 8.2 allows schema N-1 under expand/contract, and `schema_status` already tells a binary whether the schema is compatible. What that does not cover is the opposite direction: an N-1 binary that keeps serving while new data of a kind it cannot read starts to appear. Wave 2 adds rules for that, and one mechanism.

**Rules for every wave 2 migration author** (added to the four of db.md section 4):

1. A release that introduces a table or column ships it as expand only. A table the N-1 binary never reads cannot hurt it.
2. **Value widening** (a wider `CHECK` list, a new `grant_kind`, a new `blob_stores.kind`, a new `users.source`) is an expand migration, but the new value is written only after the **feature gate** of that feature is open. An N-1 binary that meets a value it cannot decode fails the whole row mapping, and for a role that contains such a privilege that means an authorization error for its users.
3. **Enforcement features** (pull gate in `enforce` mode, quarantine, read only mode, quota counters as the authority) are dormant until their gate is open, because an N-1 binary would keep serving without them. Guards that live in the database (immutability, holds, read only, scope checks, `RX004` to `RX008`) apply to every binary at once, N-1 included; an N-1 writer sees them as `StoreError::Conflict`, which is why they are safe to ship in the first release.
4. New columns have defaults or are nullable; no column the N-1 binary reads is dropped or tightened. Contract steps are listed per stream and never ship in the same release as their expand.
5. Rolling back below a feature level that created data is a documented procedure, not an accident: section 3.c lists the robot revocation step; the others only lose function, not safety.

**Mechanism.**

```rust
// core: crates/rustryx-core/src/node.rs  (new file)
/// Feature level of the running binary. Phase 1 was level 1; wave 2 ships as level 2. A later release that adds a gated
/// feature raises it by one.
pub const CURRENT_FEATURE_LEVEL: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureId {
    Projects, RobotTokens, ApplicationPrivileges, BlobGroupsAndAzure,
    PullGateEnforce, Quarantine, QuotaCounters, ReadOnlyMode,
}
impl FeatureId {
    /// The level at which every node understands the feature. All wave 2 features are 2.
    #[must_use] pub const fn required_level(self) -> u32 { 2 }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeInfo {
    pub node_id: String,
    pub version: String,
    pub feature_level: u32,
    pub started_at: OffsetDateTime,
    pub seen_at: OffsetDateTime,
    /// Highest `system_config` revision this node has applied (read only mode, settings).
    pub settings_revision: i64,
    /// Write requests in flight, for the read only quiesce check.
    pub inflight_writes: u32,
}

/// Open when every live node is at or above the level. An empty node list is closed (fail closed).
#[must_use]
pub fn gate_open(feature: FeatureId, live_nodes: &[NodeInfo]) -> bool;
```

```rust
// core: crates/rustryx-core/src/ports/system.rs  (part 3)
#[async_trait]
pub trait NodeRegistryRepo: Send + Sync {
    /// Upsert of the node's own row, every 30 s (`node_heartbeat`).
    async fn heartbeat(&self, node: &NodeInfo) -> Result<(), StoreError>;
    /// Nodes seen within `max_age` (90 s). A stopped node drops out by itself; there is no deregistration to forget.
    async fn list_live(&self, max_age: Duration) -> Result<Vec<NodeInfo>, StoreError>;
}
```

A service that is about to create gated data asks `gate_open` (cached 10 s) and answers `409 cluster_upgrade_in_progress` with the list of nodes below the level when it is closed. No operator step is involved: the gate opens by itself when the last old pod is gone. `GET /api/v1/system/nodes` (`OPS-27`) is the same data.

### 2.5 Egress, new crates, configuration

**`rustryx-egress` (new, recommended).** Scanning, replication, webhooks and notifications all make outbound calls to administrator supplied URLs, and all of them need the guarded resolver, redirect validation, breaker and timeouts that exist today inside `rustryx-format-core::proxy::{ssrf, executor, upstream, breaker}`. Depending on `format-core` for that would point `scan`, `replication` and `notify` at the format layer. The recommendation is to move that module, unchanged, into `rustryx-egress` and re-export it from format-core (no import breaks). Dependency direction: `egress -> core`; `format-core -> egress`; `scan, replication, notify -> egress, core, tasks`.

One policy object per purpose, so a webhook is never allowed what an internal mirror is:

| Purpose | Used by | Config section | Redirects | Notes |
|---|---|---|---|---|
| `Upstream` | proxy repositories | `upstream.*` (unchanged) | up to 3, each hop validated | resolves registry endpoints too when a proxy uses one |
| `Registry` | registry endpoints, replication | `egress.registries.*` | up to 3 | streaming `POST`, `PATCH`, `PUT` added; retries stay GET and HEAD only |
| `Webhook` | webhook deliveries | `egress.webhooks.*` | none | internal CIDRs empty by default (SEC-34) |
| `Scanner` | HTTP scanner adapters, data set downloads | `egress.scanners.*` | up to 3 | |
| `Smtp` | the mail relay | `egress.smtp.*` | n/a | ports 465 and 587 only by default |

Each `egress.<purpose>.*` section has the shape of `upstream.*`: `allowed_schemes` (default `["https"]`), `allow_insecure_http_hosts` (default empty), `allowed_hosts` (default empty, meaning any host that passes the address filter), `allowed_ports` (default `[443]`), `allow_private_cidrs` (default empty). Loopback, link local, metadata, multicast, unspecified and broadcast ranges can never be allowlisted, as today. One more key, `egress.proxy.allowed_hosts`, lists the hosts that the outbound proxy setting (section 8) may name; empty means the setting is refused. These are file or environment configuration, not API editable settings, on purpose: they are the security boundary, and an API administrator must not be able to widen them.

**Other new configuration keys** (each needs the `rustryx-config` key table entry, the bless step of config.md and `config.example.yaml`): `tasks.max_concurrent_runs` (4), `tasks.scheduler_tick_secs` (15), `tasks.run_retention_days` (90), `tasks.log_retention_days` (30), `outbox.retention_days` (30), `outbox.poll_interval_ms` (1000), `downloads.flush_interval_secs` (10), `downloads.retention_days` (90, at least 35), `scan.*` (section 5.g), `auth.oidc.authorized_parties` (gap 17). Operator tunable policy lives in database settings (section 8), the boundary and bootstrap values live here: that split is ADR-W2-24.

**New crates.**

| Crate | Content | Depends on |
|---|---|---|
| `rustryx-egress` | guarded outbound HTTP, per purpose policy, bandwidth limiter | core, config |
| `rustryx-scan` | `Scanner` trait, Trivy, OSV and HTTP adapters, gate evaluation, trust verification policy, SBOM, hub stats, scan worker | core, tasks, egress, blob |
| `rustryx-replication` | registry client, replication executor and worker, promotion executor | core, tasks, egress, blob, format-oci |
| `rustryx-notify` | webhook dispatcher and signer, e-mail sender, outbox consumers | core, tasks, egress, config |
| `rustryx-cli` | binary `ryx`, generated from the OpenAPI document, OIDC device flow | no server crate |

### 2.6 Metrics, cardinality and alerts

Rules for every new series (SRE-09 stays in force):

1. No project, repository, user, path, scanner name or webhook id in a label. The only free name labels are `store` (blob stores) and `repo` on `upstream_*`, as today. `job` is the catalog vocabulary. Anything per project is a database query behind the API, not a series.
2. Counters of rare events are alerted with `increase`, not `rate` (SRE-10).
3. `background_job_runs_total{job,result}` gains `skipped`. The label is `job` as in slo.md; if the rename to `task` of core-gaps ever lands, the new series follow it with the rest.

New alert rules (added to `deploy/monitoring/rustryx-alerts.yaml` by the stream that owns the series, each with `severity`, `runbook_url`, `summary`):

| Alert | Condition (sketch) | Severity | Stream |
|---|---|---|---|
| `RustryxOutboxLag` | `outbox_lag_seconds > 300` for 5 min | high | E |
| `RustryxWebhookBacklog` | `webhook_deliveries_pending > 5000` for 15 min | medium | E |
| `RustryxReplicationFailing` | `increase(replication_tasks_total{result="failed"}[1h]) > 20` | medium | E |
| `RustryxScanBacklog` | `scan_queue_depth > 1000` for 30 min | medium | D |
| `RustryxScannerDown` | `scanner_up == 0` for 15 min | high | D |
| `RustryxScannerDbStale` | `scanner_db_age_seconds > 7 * 86400` | high | D |
| `RustryxQuotaDrift` | `increase(quota_recount_corrections_total[1d]) > 0` | medium | C |
| `RustryxVerifyBlobsMissing` | `increase(verify_blobs_findings_total{kind="missing"}[1d]) > 0` | critical | F |
| `RustryxReadOnlyLong` | `read_only_enabled == 1` for 1 h | high | C |
| `RustryxSettingsSkew` | `max(settings_revision_applied) - min(settings_revision_applied) > 0` for 2 min | medium | H |
| `RustryxClusterVersionSkew` | `count(count by (feature_level)(node_feature_level)) > 1` for 1 h | medium | C |
| `RustryxPullGateErrors` | `increase(pull_gate_decisions_total{decision="error"}[10m]) > 0` | high | D |

### 2.7 Migration number ranges

No database exists in production yet, so ranges only prevent merge collisions between parallel streams; after the first release numbering follows merge order. sqlx applies every embedded version that is not yet recorded, so gaps are harmless.

| Range | Owner |
|---|---|
| 0007 | foundation (section 1.5) |
| 0020 to 0039 | W2-B |
| 0040 to 0059 | W2-C |
| 0060 to 0079 | W2-D |
| 0080 to 0099 | W2-E |
| 0100 to 0119 | W2-F |
| 0120 to 0139 | W2-H |

### 2.8 Route authorization vocabulary

SEC-11 requires every route to declare what it needs. The api crate gets one declaration type, and the route enumeration test (401 without credentials, 403 without the privilege) reads it. New routes of this document use:

```rust
// crate: rustryx-api
pub enum Requires {
    Public,                                   // login, probes, anonymous reads gated by the repository
    Authenticated,                            // /me
    Repo(Action),                             // content, through ResourceRef::for_repository
    Project(ProjectPermission),               // path carries the project id
    App(AppArea, Action),                     // administration areas, nx-admin passes
    Admin,                                    // nx-admin only
}
```

Until `ApplicationPrivileges` is open (section 2.4), `App(..)` is evaluated as `Admin`.

---

## 3. W2-B: projects, members, roles, robot tokens, labels

Capabilities: `AUTH-42`, `AUTH-55`, `AUTH-56`, `AUTH-62` to `AUTH-67`, `AUTH-82`, `OPS-24`, `UPL-10`. Decisions of parity sections 11.1, 11.2 and 11.9 are made here; the ADR table (section 11) has the alternatives.

The shape in one paragraph: a project is an optional container over repositories of any format. A repository is in at most one project. The five project roles are templates that rustryx-rbac expands; membership is a row for a user or for an identity provider group (exact group string, as in README 8.5). A robot is a scoped token with its own table. A label is one metadata mechanism with a scope.

### 3.a Entities and enums

```rust
// core: crates/rustryx-core/src/project.rs  (new file)
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::identity::{Action, IdpProvider};
use crate::ids::{ProjectId, ProjectMemberId, UserId};

/// Names that would clash with a URL space or a UI route. A project name is also an OCI path prefix
/// (`host/<project>/<image>`), so it follows the container naming rules, not the looser `RepoName` ones.
const RESERVED_NAMES: &[&str] = &[
    "api", "v2", "service", "repository", "static", "assets", "metrics", "healthz", "readyz",
    "login", "logout", "auth", "admin", "ui", "docs", "openapi",
];

/// 2 to 64 characters, lowercase letters and digits, single `.`, `_` or `-` as separators between them,
/// not in the reserved list. Stored lowercase, unique.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);
impl ProjectName {
    pub fn parse(value: &str) -> Result<Self, DomainError>;
    #[must_use] pub fn as_str(&self) -> &str;
}
// Deserialize validates through `parse`, like `RepoName`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectVisibility { Private, Public }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
    /// Markdown, at most 4000 characters. Rendered by the UI with a sanitizing renderer.
    pub description: String,
    pub visibility: ProjectVisibility,
    pub created_by: Option<UserId>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRole { LimitedGuest, Guest, Developer, Maintainer, ProjectAdmin }

/// Project level actions that are not content actions. `nx-admin` holds all of them everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectPermission {
    ViewProject, ViewQuota, ViewFindings, ViewAudit,
    RunScans, ApplyLabels, ManageLabels,
    ManagePolicies,      // retention, immutability, CVE allowlist, trust, scan and gate settings, quarantine release
    ManageWebhooks, ManageRobots, ManageMembers,
    ManageSettings,      // description, visibility, delete the project
    ApprovePromotion,    // on the target project
}

/// What a role grants. Content actions apply to every repository of the project.
#[derive(Debug, Clone, Copy)]
pub struct ProjectGrantSet {
    pub content: &'static [Action],
    pub project: &'static [ProjectPermission],
}

impl ProjectRole {
    /// The one place the role matrix lives. A test pins every cell (section 3.h).
    #[must_use]
    pub const fn template(self) -> ProjectGrantSet {
        use Action::{Browse, Delete, Read, Write};
        use ProjectPermission::*;
        match self {
            Self::LimitedGuest => ProjectGrantSet { content: &[Browse, Read], project: &[ViewProject, ViewQuota] },
            Self::Guest => ProjectGrantSet { content: &[Browse, Read],
                project: &[ViewProject, ViewQuota, ViewFindings, ViewAudit] },
            Self::Developer => ProjectGrantSet { content: &[Browse, Read, Write],
                project: &[ViewProject, ViewQuota, ViewFindings, ViewAudit, RunScans, ApplyLabels] },
            Self::Maintainer => ProjectGrantSet { content: &[Browse, Read, Write, Delete],
                project: &[ViewProject, ViewQuota, ViewFindings, ViewAudit, RunScans, ApplyLabels,
                           ManagePolicies, ManageWebhooks, ApprovePromotion] },
            Self::ProjectAdmin => ProjectGrantSet { content: &[Browse, Read, Write, Delete],
                project: &[ViewProject, ViewQuota, ViewFindings, ViewAudit, RunScans, ApplyLabels,
                           ManagePolicies, ManageWebhooks, ApprovePromotion,
                           ManageLabels, ManageRobots, ManageMembers, ManageSettings] },
        }
    }
    #[must_use] pub const fn as_str(self) -> &'static str;
    pub fn parse(label: &str) -> Result<Self, DomainError>;
}

/// Who a project role is given to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectSubject {
    User { user_id: UserId },
    /// Exact, case sensitive, untrimmed: a Keycloak full path or an LDAP DN (README 8.5). Never a prefix or a pattern.
    Group { provider: IdpProvider, external_group: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectMember {
    pub id: ProjectMemberId,
    pub project_id: ProjectId,
    pub subject: ProjectSubject,
    pub role: ProjectRole,
    pub added_by: Option<UserId>,
    pub created_at: OffsetDateTime,
}
```

Role matrix (the table the code above encodes; the starting set of `AUTH-56`, a Rustryx design, not copied from any product):

| Capability | limited guest | guest | developer | maintainer | project admin |
|---|---|---|---|---|---|
| content `browse`, `read` | yes | yes | yes | yes | yes |
| content `write` | | | yes | yes | yes |
| content `delete` | | | | yes | yes |
| `ViewProject`, `ViewQuota` | yes | yes | yes | yes | yes |
| `ViewFindings`, `ViewAudit` | | yes | yes | yes | yes |
| `RunScans`, `ApplyLabels` | | | yes | yes | yes |
| `ManagePolicies`, `ManageWebhooks`, `ApprovePromotion` | | | | yes | yes |
| `ManageLabels`, `ManageRobots`, `ManageMembers`, `ManageSettings` | | | | | yes |

Rules that are not in the matrix: a principal's effective grants in a project are the **union** of the templates of all its memberships there (direct row plus every matching group row), so order and duplicates do not matter. `nx-admin` holds every project permission. Quota limits, registries, scanners, the system CVE allowlist and global settings are never project permissions (parity 11.1): they stay `nx-admin`. A **public** project gives every principal, authenticated or not, `browse` and `read` on its repositories and `ViewProject`, nothing else; an anonymous caller still needs the global anonymous switch (README 8.5). A private project is invisible (404) to non members.

```rust
// core: crates/rustryx-core/src/label.rs  (new file)
/// 1 to 128 characters of letters, digits, `_`, `-`, `.` and `:`; first character a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LabelName(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", content = "project_id", rename_all = "snake_case")]
pub enum LabelScope { System, Project(ProjectId) }

/// One metadata mechanism (parity 11.9). The word "tag" is reserved for OCI tags; the compat tag REST resource
/// is a compatibility view of system scope labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Label {
    pub id: LabelId,
    pub scope: LabelScope,
    pub name: LabelName,
    /// `#rrggbb`, validated.
    pub color: String,
    pub description: String,
    /// JSON object, at most 20 KiB serialized (the size compat tags allow), no secrets by convention.
    pub attributes: serde_json::Value,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub struct ComponentLabel { pub component_id: ComponentId, pub label_id: LabelId,
                            pub applied_by: Option<UserId>, pub applied_at: OffsetDateTime }
```

```rust
// core: crates/rustryx-core/src/robot.rs  (new file)
/// A robot token: automation access without a person (parity 11.2). Rights come from `scopes` only; a robot has
/// no roles, never `nx-admin`, no project permissions except `scan`, and cannot log in to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RobotToken {
    pub id: RobotTokenId,
    /// Full display name, `<prefix>+<project>+<short name>` for a project robot and `<prefix>+<short name>` for a
    /// system robot. Informational: the Basic user name is ignored, exactly as for personal tokens.
    pub name: String,
    pub short_name: String,
    /// `None` for a system robot.
    pub project_id: Option<ProjectId>,
    /// Same grammar family as personal token scopes, see below. At least one, at most 64.
    pub scopes: Vec<String>,
    /// Lowercase hex SHA-256 of the whole `ryx_` token. The secret is shown once.
    pub token_hash: String,
    pub display_prefix: String,
    /// `None` only for a never-expire robot, which needs `nx-admin` and `robots.allow_never_expire`.
    pub expires_at: Option<OffsetDateTime>,
    pub disabled_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub last_used_at: Option<OffsetDateTime>,
    pub rotated_at: Option<OffsetDateTime>,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub version: i64,
}

/// A validated scope list for a robot. Parsing enforces: no `*`, at least one scope, only actions
/// `browse|read|write|delete` on `repo:` scopes and those plus `scan` on `project:` scopes, no `*` action,
/// `read` requires `browse` and `write` or `delete` require `read` on the same target (push implies pull),
/// a project robot may carry only `project:<its project id>:...` and `repo:` scopes of repositories in that
/// project (checked by the service, the type only checks the shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotScopeSet(Vec<String>);
impl RobotScopeSet {
    pub fn parse(scopes: &[String]) -> Result<Self, DomainError>;
    #[must_use] pub fn as_slice(&self) -> &[String];
}
```

Scope grammar, shared by personal tokens and robots (the `rustryx-rbac` parser grows by one production; existing `repo:` scopes keep their meaning):

```text
scope   = "*" | "repo:" target ":" action | "project:" ptarget ":" action
target  = repository name | "*"
ptarget = project id (UUID) | "*"           ids, not names: renaming a project does not change what a token can reach
action  = "browse" | "read" | "write" | "delete" | "scan" | "*"      "scan" only on project scopes
```

For a **personal** token a scope narrows the owner's rights (intersection, unchanged). For a **robot** the scopes are the rights themselves (union of the grants, within what the scopes name). `project:*:<action>` means every repository that has a project, including projects created later, and never a repository without a project.

### 3.b Ports

```rust
// core: crates/rustryx-core/src/ports/projects.rs  (new file)
// Contract as in ports/mod.rs: typed errors, one statement or transaction per method, cancel safe.

#[derive(Debug, Clone)]
pub struct ProjectSummary { pub project: Project, pub repository_count: u32, pub member_count: u32 }

#[derive(Debug, Clone)]
pub struct ProjectDependents { pub repositories: Vec<RepositoryId>, pub robots: u32, pub webhooks: u32 }

#[async_trait]
pub trait ProjectRepo: Send + Sync {
    /// Inserts the project and, when `first_admin` is given, that user as `project_admin` in the same
    /// transaction. `Conflict` when the name exists (names are lowercase, so the check is exact).
    async fn create(&self, project: &Project, first_admin: Option<UserId>) -> Result<(), StoreError>;
    async fn get(&self, id: ProjectId) -> Result<Option<Project>, StoreError>;
    async fn get_by_name(&self, name: &ProjectName) -> Result<Option<Project>, StoreError>;
    /// Keyset by name. The filter comes from `RbacChecker::visible_projects`, so a caller cannot list what it may not see.
    async fn list_visible(&self, filter: &ProjectVisibilityFilter, query: Option<&str>, page: &PageRequest)
        -> Result<Page<ProjectSummary>, StoreError>;
    /// Replaces description and visibility. A visibility change rewrites `allow_anonymous` of every repository of the
    /// project in the same transaction (trigger, section 1.5) and publishes `InvalidationEvent::Projects`.
    async fn update(&self, project: &Project, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while any repository is attached (no cascade, ADR-W2-1). Robots and members go with the project.
    async fn delete(&self, id: ProjectId) -> Result<(), StoreError>;
    async fn dependents(&self, id: ProjectId) -> Result<ProjectDependents, StoreError>;
    /// One transaction: sets `repositories.project_id`, removes project scoped label links of that repository's
    /// components that belong to another project, publishes `Projects`. `Conflict` when it is attached elsewhere.
    async fn attach_repository(&self, project: ProjectId, repository: RepositoryId) -> Result<(), StoreError>;
    /// Sets `project_id` to NULL and `allow_anonymous` to false in one statement (fail safe: a detached repository is never public by accident).
    async fn detach_repository(&self, repository: RepositoryId) -> Result<(), StoreError>;
}

#[async_trait]
pub trait ProjectMemberRepo: Send + Sync {
    async fn list(&self, project: ProjectId, page: &PageRequest) -> Result<Page<ProjectMember>, StoreError>;
    async fn get(&self, id: ProjectMemberId) -> Result<Option<ProjectMember>, StoreError>;
    /// `Conflict` when the subject already has a row in the project. Publishes `User(id)` or `Projects`.
    async fn add(&self, member: &ProjectMember) -> Result<(), StoreError>;
    /// `Conflict` when it would remove the last `project_admin` (direct or group), unless `allow_last_admin` (nx-admin).
    async fn set_role(&self, id: ProjectMemberId, role: ProjectRole, allow_last_admin: bool) -> Result<(), StoreError>;
    async fn remove(&self, id: ProjectMemberId, allow_last_admin: bool) -> Result<(), StoreError>;
}

/// Read side used only by rustryx-rbac (never by handlers): what a principal's roles in projects are.
#[async_trait]
pub trait ProjectAccessRepo: Send + Sync {
    /// Direct memberships of a user.
    async fn memberships_of_user(&self, user: UserId) -> Result<Vec<(ProjectId, ProjectRole)>, StoreError>;
    /// Every group row of a provider as `(external_group, project, role)`. Bounded: `StoreError::Invalid` when more than
    /// `limit` rows exist, so a runaway table fails closed instead of loading without bound.
    async fn group_mappings(&self, provider: IdpProvider, limit: u32)
        -> Result<Vec<(String, ProjectId, ProjectRole)>, StoreError>;
}

#[async_trait]
pub trait LabelRepo: Send + Sync {
    async fn create(&self, label: &Label) -> Result<(), StoreError>;       // Conflict on a name clash in the same scope
    async fn get(&self, id: LabelId) -> Result<Option<Label>, StoreError>;
    /// `System` lists system labels; `Project(p)` lists project labels. The UI shows system labels first.
    async fn list(&self, scope: LabelScope, page: &PageRequest) -> Result<Page<Label>, StoreError>;
    async fn update(&self, label: &Label, expected_version: i64) -> Result<(), StoreError>;
    /// Removes the label and its links. Returns how many components lost it (shown in the delete dialog).
    async fn delete(&self, id: LabelId) -> Result<u64, StoreError>;
    /// `RX008` (a project label on a component of another project) is `Conflict`. More than 50 labels on one
    /// component is `Invalid`. Idempotent for an existing link.
    async fn apply(&self, component: ComponentId, label: LabelId, by: Option<UserId>) -> Result<(), StoreError>;
    async fn unapply(&self, component: ComponentId, label: LabelId) -> Result<(), StoreError>;
    async fn labels_of(&self, component: ComponentId) -> Result<Vec<Label>, StoreError>;
    async fn usage(&self, label: LabelId) -> Result<u64, StoreError>;
}

#[derive(Debug, Clone, Default)]
pub struct RobotFilter { pub project: Option<ProjectId>, pub system_only: bool, pub state: Option<RobotState> }
pub enum RobotState { Active, Disabled, Expired, Revoked }

#[async_trait]
pub trait RobotTokenRepo: Send + Sync {
    async fn create(&self, token: &RobotToken) -> Result<(), StoreError>;     // Conflict on a duplicate name or hash
    /// Indexed lookup by SHA-256 hex (the authenticator second lookup, after `TokenRepo::find_by_hash` missed).
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<RobotToken>, StoreError>;
    async fn get(&self, id: RobotTokenId) -> Result<Option<RobotToken>, StoreError>;
    async fn list(&self, filter: &RobotFilter, page: &PageRequest) -> Result<Page<RobotToken>, StoreError>;
    /// Replaces hash, prefix and `rotated_at`. A disabled robot stays disabled. The old secret stops working at
    /// once, because only one hash is stored.
    async fn rotate(&self, id: RobotTokenId, new_hash: &str, new_prefix: &str, at: OffsetDateTime,
                    expected_version: i64) -> Result<(), StoreError>;
    async fn set_disabled(&self, id: RobotTokenId, disabled: bool, at: OffsetDateTime, expected_version: i64)
        -> Result<(), StoreError>;
    async fn revoke(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
    async fn delete(&self, id: RobotTokenId) -> Result<(), StoreError>;
    /// Coalesced by the caller to once a minute per token per node.
    async fn touch_last_used(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Deletes robots that expired or were revoked before `before`. Returns the count.
    async fn purge_expired(&self, before: OffsetDateTime) -> Result<u64, StoreError>;
    /// Tokens (robot) that expire within `within` and have no expiry notice yet, for `token_expiry_notice`.
    async fn list_expiring(&self, within: Duration, now: OffsetDateTime, limit: u32) -> Result<Vec<RobotToken>, StoreError>;
    async fn mark_expiry_notified(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
}
```

`TokenRepo` (personal tokens) gets the same two expiry methods (`list_expiring`, `mark_expiry_notified`) so `token_expiry_notice` covers both kinds. They are provided with an empty default; db overrides them.

### 3.c PostgreSQL

Migrations `0020_project_members.sql`, `0021_labels.sql`, `0022_robot_tokens.sql`. `projects` and `repositories.project_id` are in the foundation migration.

```sql
-- 0020: membership. One row per subject per project; a role change is an UPDATE.
CREATE TABLE project_members (
    id            uuid PRIMARY KEY,
    project_id    uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    subject_kind  text        NOT NULL CHECK (subject_kind IN ('user', 'group')),
    user_id       uuid        REFERENCES users (id) ON DELETE CASCADE,
    provider      text        CHECK (provider IN ('oidc', 'ldap')),
    -- Exact and case sensitive, like idp_group_mappings.external_group; never normalised.
    external_group text       CHECK (external_group <> '' AND length(external_group) <= 1024),
    role          text        NOT NULL CHECK (role IN ('limited_guest', 'guest', 'developer', 'maintainer', 'project_admin')),
    added_by      uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_project_members_shape CHECK (
        (subject_kind = 'user'  AND user_id IS NOT NULL AND provider IS NULL AND external_group IS NULL) OR
        (subject_kind = 'group' AND user_id IS NULL AND provider IS NOT NULL AND external_group IS NOT NULL))
);
CREATE UNIQUE INDEX uq_project_members_user  ON project_members (project_id, user_id) WHERE subject_kind = 'user';
CREATE UNIQUE INDEX uq_project_members_group ON project_members (project_id, provider, external_group) WHERE subject_kind = 'group';
CREATE INDEX ix_project_members_user  ON project_members (user_id) WHERE subject_kind = 'user';
CREATE INDEX ix_project_members_group ON project_members (provider, external_group) WHERE subject_kind = 'group';
-- Last project_admin guard: a BEFORE DELETE/UPDATE trigger takes pg_advisory_xact_lock(hashtextextended(project_id::text, 0))
-- first, then counts the remaining project_admin rows; none left raises RX008 unless rustryx.allow_last_admin = 'on'.

-- 0021: labels. System labels have project_id NULL.
CREATE TABLE labels (
    id          uuid PRIMARY KEY,
    project_id  uuid        REFERENCES projects (id) ON DELETE CASCADE,
    name        text        NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    color       text        NOT NULL DEFAULT '#6b7280' CHECK (color ~ '^#[0-9a-f]{6}$'),
    description text        NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    attributes  jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(attributes) = 'object' AND pg_column_size(attributes) <= 20480),
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX uq_labels_system  ON labels (name) WHERE project_id IS NULL;
CREATE UNIQUE INDEX uq_labels_project ON labels (project_id, name) WHERE project_id IS NOT NULL;

CREATE TABLE component_labels (
    component_id uuid        NOT NULL REFERENCES components (id) ON DELETE CASCADE,
    label_id     uuid        NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    applied_by   uuid        REFERENCES users (id) ON DELETE SET NULL,
    applied_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (component_id, label_id)
);
CREATE INDEX ix_component_labels_label ON component_labels (label_id);
-- Scope guard (SQLSTATE RX008): a project label can be applied only to components of a repository of that project,
-- and at most 50 labels per component. Written as a BEFORE INSERT trigger, so no code path can bypass it.

-- 0022: robots. A separate table from user_tokens, ADR-W2-2.
CREATE TABLE robot_tokens (
    id             uuid PRIMARY KEY,
    name           text        NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9+._-]{0,127}$'),
    short_name     text        NOT NULL CHECK (short_name ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    project_id     uuid        REFERENCES projects (id) ON DELETE CASCADE,
    scopes         text[]      NOT NULL CHECK (cardinality(scopes) BETWEEN 1 AND 64),
    token_hash     text        NOT NULL CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    display_prefix text        NOT NULL,
    expires_at     timestamptz,
    disabled_at    timestamptz,
    revoked_at     timestamptz,
    last_used_at   timestamptz,
    rotated_at     timestamptz,
    expiry_notice_at timestamptz,
    created_by     uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    version        bigint      NOT NULL DEFAULT 1,
    CONSTRAINT uq_robot_tokens_hash UNIQUE (token_hash)
);
CREATE UNIQUE INDEX uq_robot_tokens_name ON robot_tokens (lower(name));
CREATE INDEX ix_robot_tokens_project ON robot_tokens (project_id) WHERE project_id IS NOT NULL;
CREATE INDEX ix_robot_tokens_expiry ON robot_tokens (expires_at) WHERE expires_at IS NOT NULL AND revoked_at IS NULL;
ALTER TABLE user_tokens ADD COLUMN expiry_notice_at timestamptz;
```

Index choices: the group index of rbac loads `project_members` by `(provider, external_group)` wholesale and keeps it in memory (section 3.e), so the partial group index serves the bounded scan; `ix_project_members_user` serves the per user cache load.

Retention: `robot_tokens` rows live until purged (`robots.expired_retention_days`, default 30 after expiry or revocation); the audit rows keep the history. `project_members`, `labels` and `component_labels` have no retention; they die with their parents.

Expand and contract. All three migrations are pure expand (new tables, one nullable column) and ship in release N together with the code. The gates: creating the first project, the first group row or the first robot waits for `Projects` and `RobotTokens` (section 2.4), because an N-1 pod would ignore project roles (members would be refused there) and would answer 401 for a robot token (it only knows `user_tokens`). Both failures are clean refusals, never a privilege confusion: an N-1 pod cannot find a robot token and does not read project tables. No contract step exists for this stream. **Rollback below level 2** after robots were created: an N-1 pod does not know `robot_tokens`, so every robot token simply fails with 401 there and nothing is exposed; the runbook step is only to tell the owners of CI jobs, and to revoke robots (`POST /api/v1/api-tokens/actions/revoke-robots`) when the rollback is permanent so that a later upgrade does not revive old secrets.

### 3.d Jobs

| Job | What it does | Idempotency | Metrics |
|---|---|---|---|
| `robot_token_purge` (daily 03:30, lock `task:robot_token_purge`) | `RobotTokenRepo::purge_expired(now - robots.expired_retention_days)`; the same for personal tokens through `TokenRepo`. Audit row `robot.purge` with the count | a repeated run deletes nothing new | `token_ops_total{kind,op}` with `op="purge"` |
| `token_expiry_notice` (daily 06:00, lock `task:token_expiry_notice`) | tokens that expire within 7 days and have `expiry_notice_at IS NULL`: one `token.expiring` outbox event each (consumed by e-mail, section 6), then `mark_expiry_notified` in the same transaction | the mark makes a second run skip them | `token_ops_total{op="expiry_notice"}` |

Runtime counters (no job): `token_ops_total{kind="robot|personal",op="create|rotate|disable|enable|revoke|delete|purge|expiry_notice"}`, `robot_auth_total{result="ok|unknown|disabled|expired|revoked|scope_denied"}`, `project_authz_cache_total{cache="direct|groups",result="hit|miss"}`.

### 3.e Authorization

Everything below is what `rustryx-rbac` implements behind the provided methods of section 1.3.6. There is still exactly one authorization engine; projects add inputs to it, not a second engine.

**Decision order v2** for `RbacChecker::check(principal, action, resource)`; the first rule that decides wins:

1. Anonymous `Write` or `Delete`: deny (unchanged). An anonymous caller is also denied unless `resource.allow_anonymous` is true **and** the anonymous switch of `Rbac` is on (gap 13). Both gates now live in `check`.
2. Token scopes: if `principal.token_scopes` is non empty and the principal is **not** a robot, an action outside the scopes is denied (unchanged; the grammar gained `project:`).
3. `nx-admin` by role id: allow (unchanged, `ADR-RBAC-4`).
4. Robot or scan token (`AuthMethod::RobotToken`, `ScanToken`): the grants are read from `token_scopes` and nothing else is consulted. `repo:R:A` allows `A` on repository `R`; `project:P:A` allows `A` on repositories whose `resource.project.id` is `P`; `project:*:A` on every repository that has a project. `Admin` and `Application` grants never apply to a robot, and it has no roles. Deny otherwise. (A robot never reaches rules 5 to 7.)
5. Privileges of the principal's roles (unchanged: `RepositoryView`, `RepositoryContentSelector`, `Admin`; `Application` grants do not apply to content).
6. **Project roles (new).** If `resource.project` is `Some(p)`: allow when `action` is in the union of the content sets of the principal's roles in `p`, or when `p.visibility` is `Public` and `action` is `Browse` or `Read`.
7. Deny.

Roles of a principal in a project are computed per request from two inputs that both come from the principal of *this* request:

* direct rows of `principal.user_id` (cache `direct`, key `(UserId, version)`, value `Arc<HashMap<ProjectId, ProjectRoleSet>>`, loaded from `ProjectAccessRepo::memberships_of_user`);
* group rows matching `principal.external_groups`, for the provider selected by `principal.source` (`Oidc` to `IdpProvider::Oidc`, `Ldap` to `IdpProvider::Ldap`, `Local` to none). Matching is exact on the whole string: no trimming, no case folding, no prefix, as README 8.5. The cache `groups`, key `(IdpProvider, version)`, holds the whole provider index `HashMap<String, Vec<(ProjectId, ProjectRole)>>` loaded by `group_mappings(provider, limit)` (default limit 100 000 rows; more fails closed). A request then does one hash lookup per group of the principal, with no per user cache keyed by a group list (a list of up to 1000 strings is a bad key, and a hash of it would risk the collision that rbac.md section 'caching' forbids).

A principal whose groups cannot be confirmed never reaches here: the auth chain refuses the request (LDAP older than 5 minutes, README 8.5). An API token of an OIDC user carries no groups (`auth-core.md` ADR-3), so such a token gets direct memberships only, which is the existing rule applied to projects.

**Project level checks** `check_project(principal, project, permission)`:

* `nx-admin` (unrestricted token): allow. Anonymous: only `ViewProject` on a public project when the global switch is on.
* Robot: only `RunScans`, and only with a matching `project:<id|*>:scan` scope. Scan token: deny.
* A **restricted** personal token (non empty scopes without a lone `*`) gets **no** project permissions. A token that wants to manage a project is unrestricted or it does not. This is deliberate: the scope grammar describes content, and silently mapping content scopes to management rights would widen them.
* Everyone else: the permission must be in the union of the templates of their memberships in that project, or be `ViewProject` on a public project.
* A failed membership load is `Err(AuthError::Backend)`, never `Allow` (same contract as the rest of rbac).

**Visibility of projects**: `visible_projects` returns `All` for `nx-admin`, `Member { user_id, provider, groups }` for others (the SQL of `list_visible` is public projects plus rows matching user or `(provider, groups)`), and `PublicOnly` for anonymous. A private project the caller is not in answers 404 on every project route.

**Cache keys, TTL and invalidation** (the effect on `rustryx-rbac`; the existing caches are untouched):

| Cache | Key | Value | TTL | Capacity | Dropped by |
|---|---|---|---|---|---|
| privileges (existing) | sorted role ids, user id, version | `Arc<[Privilege]>` | 60 s | 10 000 | `Rbac`, `User(id)`, `Reset` (unchanged) |
| `direct` (new) | user id, version | per project role set | 60 s | 10 000 | `User(id)` for that user, `Projects`, `Reset` |
| `groups` (new) | provider, version | provider wide group index | 60 s | 2 | `Projects`, `Reset` |

TTL never exceeds 5 minutes (`MAX_DECISION_TTL`). The version counter is bumped by `Rbac`, `Projects` and `Reset`, and a load reads it before querying, exactly as in rbac.md, so a slow load cannot re-insert stale data after an invalidation. `ProjectMemberRepo::add`, `set_role` and `remove` publish `User(id)` for a user row and `Projects` for a group row; `ProjectRepo::update`, `attach_repository` and `detach_repository` publish `Projects`. An N-1 node decodes the unknown `projects` payload as `Reset`, which is safe.

**Who may call what** (the `Requires` declarations of section 2.8):

| Resource | Operation | Requires |
|---|---|---|
| projects | list | `Authenticated` (the filter does the rest) |
| projects | create | `nx-admin`, or any authenticated user when `settings.projects.creation = everyone`; the creator becomes `project_admin` |
| projects | read | `Project(ViewProject)` |
| projects | update description, visibility | `Project(ManageSettings)` |
| projects | delete | `Project(ManageSettings)`; refused (409 with the dependents list) while a repository is attached |
| project repositories | attach, detach | `Project(ManageSettings)` and `Repo(admin)` of the repository (phase 1: `nx-admin`) |
| members | list | `Project(ViewProject)` |
| members | add, change, remove | `Project(ManageMembers)`; see escalation rules |
| labels | list | `Project(ViewProject)`; system labels need `Authenticated` |
| labels | create, edit, delete project label | `Project(ManageLabels)`; system label: `nx-admin` |
| labels | apply, unapply | `Project(ApplyLabels)` on the component's project; system labels need `Repo(write)` on the component's repository |
| robots | list, create, rotate, disable, revoke | `Project(ManageRobots)` for a project robot, `App(Tokens, Write)` (phase 1: `nx-admin`) for a system robot |

**Escalation rules** (SEC-14, applied to this stream):

1. A project admin may grant only the five project roles, only inside its own project. Project roles contain no `nx-admin` and no application privilege, so there is nothing to escalate to.
2. A **group row** widens access to people the project admin does not manage, so it is policed by `settings.projects.group_mapping`: unless `project_admins_may_map` is true, only `nx-admin` adds group rows; when it is true, the group string must start with one of `oidc_path_prefixes` (for Keycloak paths) or end with one of `ldap_dn_suffixes` (for DNs). The default is `project_admins_may_map: false` (section 8).
3. A robot's scopes must be a subset of the creator's rights **at creation**: each `repo:R:A` needs `RbacChecker::holds_whole_repository(creator, R, A)` and each `project:P:A` needs the matching template content action (or `RunScans` for `scan`) in `P`. A creator with only a selector privilege on `R` cannot delegate it. A robot is not re-validated at use (the same behaviour as the reference products), but a status check lists robots whose creator was disabled or lost the rights (section 8).
4. The last `project_admin` cannot be removed or demoted except by `nx-admin` (trigger, section 3.c).
5. Robot principals are refused on the session login route, on every route that needs `Requires::Authenticated` for UI state (`/me`), and on all `Project(..)` permissions except `RunScans`.
6. A never-expiring robot needs `nx-admin`, `robots.allow_never_expire = true`, and is listed in the status checks. Otherwise `expires_at` is mandatory: default 30 days (`robots.default_ttl_days`), at most `robots.max_ttl_days` (default 365).

### 3.f Events and audit

No outbox event except `token.expiring`. Everything else is an audit row written by the service through `AuditSink` (never a secret, token or scope text beyond names; the robot name and id are enough).

| Audit `action` | Kind | Fields in `detail` |
|---|---|---|
| `project.create`, `project.update`, `project.delete` | `ProjectChange` | name, visibility |
| `project.visibility_change` | `ProjectChange` | from, to, repositories affected |
| `project.repository_attach`, `project.repository_detach` | `ProjectChange` | repository name |
| `project.member_add`, `project.member_update`, `project.member_remove` | `ProjectChange` | subject kind, user name or group string, role (old and new) |
| `label.create`, `label.update`, `label.delete`, `label.apply`, `label.unapply` | `ProjectChange` | label name and scope, component coordinate for apply |
| `robot.create`, `robot.rotate`, `robot.disable`, `robot.enable`, `robot.revoke`, `robot.delete`, `robot.purge` | `TokenChange` | robot name and id, project, scope summary (counts per action), expiry |
| `robot.auth_denied` | `TokenChange` | robot id, reason (`disabled`, `expired`, `revoked`); at most one row per token per minute |
| `project.authz_denied` | `SecurityDecision` | project, permission (only for admin facing routes) |

`AuditEvent.project_id` is set on all project rows, which is what the project log view (`OPS-12`) filters on.

### 3.g Crates that change

| Crate | Change |
|---|---|
| `rustryx-core` | `project.rs`, `label.rs`, `robot.rs`, `ports/projects.rs`; `Repository.project`; `ResourceRef`; `Principal`; `AuthMethod::RobotToken` |
| `rustryx-db` | repositories for the five ports, migrations 0020 to 0022, SQL for `list_visible`, triggers, `Projects` bus payload, `TokenRepo` expiry methods |
| `rustryx-rbac` | project and robot rules, the two caches, the `project:` scope production, `is_admin` and the provided trait methods |
| `rustryx-auth` | `TokenAuthenticator` second lookup in `robot_tokens` and `Principal` construction for robots (rights from scopes, `last_used_at` coalescing); session login refuses robot principals; `create_robot_token` beside `create_token`; no change to `RoleMapper` |
| `rustryx-format-core` | `ResourceRef::for_repository` everywhere authorization is built (mechanical) |
| `rustryx-api` | `/api/v1/projects`, `/projects/{id}/members`, `/projects/{id}/repositories`, `/labels`, `/components/{id}/labels`, `/api-tokens?kind=robot`, `/projects/{id}/robots`, compat `/service/rest/v1/tags` (system labels) |
| `rustryx-testkit` | in memory `MemStore` impls of the new ports, with the same guards (scope check, last admin) so rbac and api tests do not need PostgreSQL |
| `rustryx-observability` | the metric names above |

REST resources (for W2-A to put in the OpenAPI document): `GET/POST /api/v1/projects`, `GET/PUT/DELETE /api/v1/projects/{id}`, `GET/POST /api/v1/projects/{id}/members`, `PUT/DELETE /api/v1/projects/{id}/members/{memberId}`, `POST /api/v1/projects/{id}/group-convention` (creates the four explicit group rows `<prefix>/readers`, `<prefix>/publishers`, `<prefix>/maintainers`, `<prefix>/admins` for `guest`, `developer`, `maintainer`, `project_admin`, subject to the group mapping policy; it writes rows, it never matches by convention at request time), `GET/POST /api/v1/labels`, `PUT/DELETE /api/v1/labels/{id}`, `PUT/DELETE /api/v1/components/{id}/labels/{labelId}`, `GET/POST /api/v1/api-tokens?kind=robot&project=`, `POST /api/v1/api-tokens/{id}/actions/rotate`, `.../disable`, `.../enable`.

### 3.h Negative requirements (`must_not_*` tests)

| Requirement | Test |
|---|---|
| A project role never applies to a repository of another project or to a repository without a project | `rbac::must_not_apply_project_role_outside_its_project`, `..._to_a_repository_without_project` |
| A project admin never becomes `nx-admin` or gets an application privilege | `rbac::must_not_grant_admin_through_a_project_role` |
| A public project grants only `browse` and `read`, never `write`, and not to other projects | `rbac::must_not_let_public_visibility_grant_write`, `..._leak_to_other_projects` |
| Anonymous needs the repository flag and the global switch, inside `check` | `rbac::must_not_allow_anonymous_without_both_switches` |
| Group rows match exactly: not by prefix, case, trimming or substring | `rbac::must_not_match_project_group_by_prefix_case_or_whitespace` |
| An unconfirmed or failed membership load is a refusal, not an allow | `rbac::must_not_allow_when_membership_load_fails`, `..._serve_a_stale_membership_after_ttl` |
| A project admin cannot map a group outside the allowed prefixes or suffixes | `api::must_not_let_project_admin_map_a_group_outside_the_policy` |
| A project admin cannot add a role they could not hold (no role above `project_admin` exists) and cannot touch another project | `api::must_not_let_project_admin_edit_another_project` |
| The last project admin cannot be removed except by `nx-admin` | `db::must_not_remove_the_last_project_admin`, `api::..._demote_...` |
| A restricted personal token has no project permissions | `rbac::must_not_give_a_restricted_token_project_permissions` |
| A robot has no project permission except scan, no roles, no admin, no session login | `rbac::must_not_give_a_robot_project_management`, `auth::must_not_log_a_robot_in_to_the_ui` |
| A robot's secret is returned once and never again, not in list, get, rotate responses, logs or audit rows | `api::must_not_return_a_robot_secret_after_creation`, `..._log_a_robot_secret` |
| A robot cannot be created beyond the creator's rights, nor with a selector only privilege | `api::must_not_create_a_robot_beyond_the_creator`, `..._delegate_a_selector_privilege` |
| A robot scope that breaks "push implies pull" is rejected at creation | `core::must_not_accept_write_without_read_in_a_robot_scope` |
| Rotation invalidates the old secret at once; a disabled, revoked or expired robot is refused | `auth::must_not_accept_the_old_secret_after_rotation`, `..._a_disabled_robot` |
| Never-expire needs `nx-admin` and the setting | `api::must_not_create_a_never_expiring_robot_without_admin_and_setting` |
| A project label cannot be applied to a component of another project, also by SQL | `db::must_not_apply_a_label_across_projects` |
| Moving a repository to another project drops the old project's labels on its components | `db::must_not_keep_old_project_labels_after_attach` |
| A detached repository is not public | `db::must_not_leave_a_detached_repository_anonymous` |
| An N-1 write cannot make a project repository public or private against its project | `db::must_not_let_a_stale_writer_override_project_visibility` |
| The role matrix does not drift | `core::project_role_matrix_is_pinned` (every cell) |
| Two gates: project and robot creation refused while an old node is live | `api::must_not_create_a_project_while_an_older_node_is_live` |

---

## 4. W2-C: lifecycle and capacity

Capabilities: `BLOB-12`, `BLOB-20`, `RET-05` to `RET-13`, `RET-14`, `RET-17` to `RET-19`, `OPS-22`, `UPL-11`. Decisions of parity sections 11.3, 11.4, 11.5, 11.11, 11.12, 12.2 and 12.3 are made here.

Sub-steps, because other streams wait for the first: **C0** the task framework of section 2.3, the outbox of section 2.2 and the node registry of section 2.4 (about two to three weeks, the first deliverable of the whole wave); **C1** download counters, quotas, immutability, holds, read only mode; **C2** cleanup and retention engine, GC history and dry run, label cleanup.

The unit the lifecycle engine works on is a **component** for every format. For OCI a component is one manifest digest; its tags are rows owned by the OCI crate (see 4.g for the contract the OCI crate must fulfil). Retention evaluates tags and acts on whole components.

### 4.a Entities and enums

```rust
// core: crates/rustryx-core/src/quota.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum QuotaScope { Project(ProjectId), Repository(RepositoryId) }

/// A hard limit. `limit_bytes: None` is unlimited (the "minus one" of the reference product). A project limit is stored in
/// `quotas`; a repository limit stays in `repositories.quota_bytes`, so the phase 1 column keeps its meaning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Quota {
    pub scope: QuotaScope,
    pub limit_bytes: Option<u64>,
    /// Edge for the `quota.near_threshold` event, 1 to 100.
    pub warn_percent: u8,
    pub version: i64,
}

/// Distinct blobs referenced by the scope: a blob shared by several assets of one scope counts once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct QuotaUsage {
    pub scope: QuotaScope,
    pub used_bytes: u64,
    pub object_count: u64,
    pub limit_bytes: Option<u64>,
    pub warn_percent: u8,
    pub recounted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaExceeded {
    pub scope: QuotaScope,
    pub limit_bytes: u64,
    pub used_bytes: u64,
    pub requested_bytes: u64,
}

/// Room left for a new write, the smallest over the scopes that apply to a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headroom { pub bytes: Option<u64>, pub binding: Option<QuotaScope> }
```

```rust
// core: crates/rustryx-core/src/lifecycle.rs  (new file)
/// Where a policy applies. Resolution for scoped policies is repository over project over system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum PolicyScope { System, Project(ProjectId), Repository(RepositoryId) }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorMode { Matching, Excluding }

/// Glob selector. Syntax: `*` (any characters except `/`), `**` (any characters), `?` (one character except `/`),
/// `{a,b}` (at most 8 alternatives, no nesting). At most 16 globs of at most 256 characters, at most 8 wildcards each.
/// `Matching` needs at least one glob; `Excluding` may be empty and then matches everything. Rust translates every glob
/// to an anchored regular expression made only of escaped literals, `[^/]*`, `.*` and `[^/]`, which PostgreSQL evaluates
/// in bounded time; the translation is stored next to the glob (section 4.c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameSelector { pub mode: SelectorMode, pub globs: Vec<String> }

/// What an immutability rule and a retention rule compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind { Tag, Version, Path }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType { Release, Prerelease }

/// compat style policy: all set criteria must match (AND) for a component to be deleted. At least one criterion is required.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupCriteria {
    pub max_age_days: Option<u32>,
    /// Days since the last download; the publish date counts when it was never downloaded (`RET-09`).
    pub not_downloaded_days: Option<u32>,
    /// Keep the newest N versions of each package, delete the rest (`RET-07`). Version order comes from the format adapter.
    pub keep_latest_versions: Option<u32>,
    pub release_type: Option<ReleaseType>,
    /// Linear time regex against the component name (OCI: against each tag), at most 256 bytes.
    pub name_regex: Option<String>,
    /// Format specific (Maven snapshot template: minimum count, grace days, remove after release), validated by the
    /// format's `LifecycleAdapter` when the policy is saved.
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CleanupPolicy {
    pub id: CleanupPolicyId,
    pub name: String,
    pub description: String,
    /// `None` means any format; criteria a format does not support are refused when the policy is attached.
    pub format: Option<RepoFormat>,
    pub criteria: CleanupCriteria,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Harbor style retention: rules say what to KEEP; items a rule covers and no rule keeps are deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "template", rename_all = "snake_case")]
pub enum RetentionTemplate {
    KeepLastPushed { count: u32 },
    KeepLastPulled { count: u32 },
    KeepPushedWithinDays { days: u32 },
    KeepPulledWithinDays { days: u32 },
    /// Non OCI formats: newest N versions (`RET-07`).
    KeepLatestVersions { count: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionRule {
    pub template: RetentionTemplate,
    pub repository: NameSelector,
    /// Tags (OCI), versions or paths, as the format's `LifecycleAdapter::item_kind` says.
    pub item: NameSelector,
    /// OCI only: whether untagged components are covered by this rule.
    pub include_untagged: bool,
}

/// At most one per scope (project or repository), 1 to 15 rules. The schedule is a `task_definitions` row of kind
/// `retention` that the service creates and updates together with the policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RetentionPolicy {
    pub id: RetentionPolicyId,
    pub scope: PolicyScope,   // never System
    pub name: String,
    pub rules: Vec<RetentionRule>,
    /// A policy never deletes the last remaining version of a package unless this is true.
    pub allow_delete_last_version: bool,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKind { Cleanup, Retention, LabelCleanup }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemDecision { Delete, DeleteFailed, ProtectedImmutable, ProtectedHold, ProtectedLastVersion }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleCounts {
    pub evaluated: u64, pub selected: u64, pub protected_immutable: u64, pub protected_hold: u64,
    pub protected_last_version: u64, pub deleted: u64, pub failed: u64, pub bytes_selected: u64, pub bytes_deleted: u64,
}

/// One execution of a policy. A dry run stores the plan; a real run stores what it deleted. The rows are the authoritative
/// deletion record (the audit batch row only points at the run id), so a failing audit sink cannot lose it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LifecycleRun {
    pub id: LifecycleRunId,
    pub kind: LifecycleKind,
    pub cleanup_policy_id: Option<CleanupPolicyId>,
    pub retention_policy_id: Option<RetentionPolicyId>,
    pub repository_id: Option<RepositoryId>,
    pub dry_run: bool,
    /// Version of the policy this run evaluated. A real run needs a finished dry run of the same version.
    pub policy_version: i64,
    pub task_run_id: Option<TaskRunId>,
    pub status: TaskRunStatus,
    pub counts: LifecycleCounts,
    /// True when more than the per run item cap (100 000) rows were selected; the counts stay exact.
    pub truncated: bool,
    pub requested_by: Option<UserId>,
    /// An `nx-admin` confirmed a run that tripped the mass delete brake.
    pub confirmed_mass_delete: bool,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleItem {
    pub run_id: LifecycleRunId,
    pub seq: u64,
    pub repository: String,
    pub component_id: ComponentId,
    pub coordinate: String,
    pub tags: Vec<String>,
    pub size_bytes: u64,
    pub decision: ItemDecision,
    /// Stable code: the rule index, the criterion, or the protecting rule or hold id. No free text from users.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    /// Blocks lifecycle jobs, promotion moves, replication deletes and interactive deletes and overwrites.
    Legal,
    /// A release repository standing hold: blocks lifecycle jobs and promotion moves (DSO-34).
    Release,
    /// Taken automatically on the source of a running promotion, released when it ends.
    Promotion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum HoldTarget { Component(ComponentId), Repository(RepositoryId), Project(ProjectId) }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LifecycleHold {
    pub id: LifecycleHoldId,
    pub kind: HoldKind,
    pub target: HoldTarget,
    /// Required, 1 to 500 characters.
    pub reason: String,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    /// Active while `released_at` is unset and `expires_at` is unset or in the future.
    pub expires_at: Option<OffsetDateTime>,
    pub released_at: Option<OffsetDateTime>,
    pub released_by: Option<UserId>,
}

/// A rule that makes matching items unchangeable: no overwrite, delete, retag or replacement by replication or promotion.
/// Matching is SQL evaluable (globs only, not CEL), because the check runs inside the publish transaction (ADR-W2-10).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ImmutabilityRule {
    pub id: ImmutabilityRuleId,
    pub scope: PolicyScope,   // Project or Repository, never System
    pub name: String,
    pub item_kind: ItemKind,
    pub repository: NameSelector,
    pub item: NameSelector,
    pub enabled: bool,
    pub version: i64,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoftQuotaKind { MaxUsed, MinFree }
/// Alert only quota of a blob store (`BLOB-12`). The hard limit stays `BlobStoreDef.quota_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftQuota { pub kind: SoftQuotaKind, pub bytes: u64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcOptions {
    pub dry_run: bool,
    /// OCI: also delete manifests that have no tag for longer than `min_age`, are not a subject's referrer and are not a child of an index.
    pub delete_untagged: bool,
    /// Never below `gc.min_age_secs` of the configuration (validated), default equal to it.
    pub min_age_secs: u64,
    /// Parallel sweeps, 1 to 8, default 2.
    pub workers: u8,
    pub blob_store: Option<BlobStoreId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcCounts { pub examined: u64, pub candidates: u64, pub deleted: u64, pub failed: u64,
                      pub bytes_candidates: u64, pub bytes_deleted: u64, pub untagged_deleted: u64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GcRun {
    pub id: GcRunId,
    pub trigger: TaskTrigger,
    pub options: GcOptions,
    pub status: TaskRunStatus,
    pub counts: GcCounts,
    /// Per blob store breakdown of `counts`, keyed by store name.
    pub per_store: serde_json::Value,
    pub stop_requested: bool,
    pub task_run_id: Option<TaskRunId>,
    pub requested_by: Option<UserId>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

pub struct GcSample { pub run_id: GcRunId, pub blob_store: String, pub key: String, pub size_bytes: u64, pub orphaned_at: OffsetDateTime }
```

### 4.b Ports

```rust
// core: crates/rustryx-core/src/ports/lifecycle.rs  (new file)
// Same contract as the other metadata ports. Methods that enforce policies return MutationError and are atomic.

#[async_trait]
pub trait QuotaRepo: Send + Sync {
    /// Limit and usage; `None` when the scope has neither (no row, no limit).
    async fn get(&self, scope: QuotaScope) -> Result<Option<QuotaUsage>, StoreError>;
    /// Project scope writes `quotas` (and the mirror in `quota_usage`); repository scope writes `repositories.quota_bytes`.
    async fn set_limit(&self, scope: QuotaScope, limit_bytes: Option<u64>, warn_percent: u8,
                       expected_version: Option<i64>, actor: &ActorRef) -> Result<(), StoreError>;
    /// The pre check: the smallest remaining room over the repository and its project, from the counters.
    /// Used before the body is read, with the declared length. It is advisory: the authority is `publish_with`.
    async fn headroom(&self, repository: RepositoryId) -> Result<Headroom, StoreError>;
    /// Recomputes the usage from the assets (distinct blobs) with an extended statement timeout and corrects the counter.
    /// Returns `(before, after)`. A difference is counted in `quota_recount_corrections_total`.
    async fn recount(&self, scope: QuotaScope) -> Result<(u64, u64), StoreError>;
    async fn list(&self, page: &PageRequest) -> Result<Page<QuotaUsage>, StoreError>;
}

#[async_trait]
pub trait ImmutabilityRepo: Send + Sync {
    async fn create(&self, rule: &ImmutabilityRule) -> Result<(), StoreError>;   // at most 15 per scope: Conflict beyond
    async fn get(&self, id: ImmutabilityRuleId) -> Result<Option<ImmutabilityRule>, StoreError>;
    async fn list(&self, scope: PolicyScope, page: &PageRequest) -> Result<Page<ImmutabilityRule>, StoreError>;
    async fn update(&self, rule: &ImmutabilityRule, expected_version: i64) -> Result<(), StoreError>;
    async fn delete(&self, id: ImmutabilityRuleId) -> Result<(), StoreError>;
    /// The first enabled rule that covers the item, or `None`. Lifecycle jobs, promotion and the UI preview use it;
    /// publishing uses the SQL guard itself, not this call.
    async fn covering_rule(&self, repository: RepositoryId, kind: ItemKind, item: &str)
        -> Result<Option<ImmutabilityRuleId>, StoreError>;
    /// Up to `limit` existing items the rule would cover, for the editor preview.
    async fn preview(&self, rule: &ImmutabilityRule, limit: u32) -> Result<Vec<String>, StoreError>;
}

#[async_trait]
pub trait LifecycleHoldRepo: Send + Sync {
    async fn create(&self, hold: &LifecycleHold) -> Result<(), StoreError>;
    async fn get(&self, id: LifecycleHoldId) -> Result<Option<LifecycleHold>, StoreError>;
    async fn release(&self, id: LifecycleHoldId, by: Option<UserId>, at: OffsetDateTime) -> Result<(), StoreError>;
    async fn list(&self, target: Option<HoldTarget>, active_only: bool, page: &PageRequest)
        -> Result<Page<LifecycleHold>, StoreError>;
    /// Active holds that cover a component (its own, its repository's, its project's).
    async fn active_for(&self, component: ComponentId) -> Result<Vec<LifecycleHold>, StoreError>;
}

#[async_trait]
pub trait CleanupPolicyRepo: Send + Sync {            // gap 19
    async fn create(&self, policy: &CleanupPolicy) -> Result<(), StoreError>;
    async fn get(&self, id: CleanupPolicyId) -> Result<Option<CleanupPolicy>, StoreError>;
    async fn list(&self) -> Result<Vec<CleanupPolicy>, StoreError>;
    async fn update(&self, policy: &CleanupPolicy, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a repository lists the policy in `cleanup_policy_ids` (the array has no foreign key, so the
    /// port checks it with the GIN index and the caller shows `repositories_using`).
    async fn delete(&self, id: CleanupPolicyId) -> Result<(), StoreError>;
    async fn repositories_using(&self, id: CleanupPolicyId) -> Result<Vec<RepositoryId>, StoreError>;
}

#[async_trait]
pub trait RetentionPolicyRepo: Send + Sync {
    async fn upsert(&self, policy: &RetentionPolicy, expected_version: Option<i64>) -> Result<(), StoreError>;
    async fn get(&self, id: RetentionPolicyId) -> Result<Option<RetentionPolicy>, StoreError>;
    async fn get_for_scope(&self, scope: PolicyScope) -> Result<Option<RetentionPolicy>, StoreError>;
    async fn delete(&self, id: RetentionPolicyId) -> Result<(), StoreError>;
}

/// Where the engine reads candidates from.
#[derive(Debug, Clone, Copy)]
pub enum LifecycleScope { Repository(RepositoryId), Project(ProjectId) }

/// One component with everything a policy can ask about it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Candidate {
    pub component_id: ComponentId,
    pub repository_id: RepositoryId,
    pub repository: String,
    pub group_id: Option<String>,
    pub name: String,
    pub version: Option<String>,
    /// OCI tags pointing at this component; empty for other formats and for untagged manifests.
    pub tags: Vec<String>,
    pub size_bytes: u64,
    pub published_at: OffsetDateTime,
    /// Latest push of any tag (OCI) or `published_at`.
    pub pushed_at: OffsetDateTime,
    pub pulled_at: Option<OffsetDateTime>,
    /// Signature or SBOM referrer: follows its subject and is never a candidate on its own.
    pub is_referrer: bool,
}

#[async_trait]
pub trait LifecycleRepo: Send + Sync {
    /// Keyset paging over components ordered by `(repository_id, group_id, name, published_at DESC, id)`, so a streaming
    /// engine holds only the versions of one package at a time (more than 100 000 versions of one name is `Invalid`).
    /// Referrers are excluded. Cancel safe, read only.
    async fn candidates(&self, scope: LifecycleScope, page: &PageRequest) -> Result<Page<Candidate>, StoreError>;
    /// Deletes a batch (at most 500) in one transaction. Inside it, for each component: the immutability guard and legal
    /// hold guard run again unless `options.override_immutable`, referrers are deleted with their subject, the rows of
    /// `lifecycle_run_items` for this batch are written, and the outbox events of `options.events` are inserted. A protected
    /// component is reported as `Skipped`, it does not abort the batch. Returns one result per input, in order.
    async fn apply_deletions(&self, run: LifecycleRunId, batch: &[ComponentId], options: &WriteOptions)
        -> Result<Vec<DeleteResult>, StoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteResult { Deleted { bytes: u64 }, Skipped(ItemDecision), Failed }

#[async_trait]
pub trait LifecycleRunRepo: Send + Sync {
    async fn create(&self, run: &LifecycleRun) -> Result<(), StoreError>;
    async fn update_progress(&self, id: LifecycleRunId, counts: &LifecycleCounts) -> Result<(), StoreError>;
    async fn finish(&self, id: LifecycleRunId, status: TaskRunStatus, counts: &LifecycleCounts, truncated: bool,
                    at: OffsetDateTime) -> Result<(), StoreError>;
    async fn get(&self, id: LifecycleRunId) -> Result<Option<LifecycleRun>, StoreError>;
    async fn list(&self, kind: Option<LifecycleKind>, policy: Option<PolicyRef>, page: &PageRequest)
        -> Result<Page<LifecycleRun>, StoreError>;
    /// Stores plan items of a dry run, at most 100 000 per run; returns `true` when the cap cut the list.
    async fn save_items(&self, run: LifecycleRunId, items: &[LifecycleItem]) -> Result<bool, StoreError>;
    async fn list_items(&self, run: LifecycleRunId, page: &PageRequest) -> Result<Page<LifecycleItem>, StoreError>;
    /// Streams every item, for the CSV export. Backpressured.
    fn stream_items(&self, run: LifecycleRunId) -> BoxStream<'static, Result<LifecycleItem, StoreError>>;
    /// The newest finished dry run of that policy at that version, started less than `max_age` ago. Gate of the real run.
    async fn last_dry_run(&self, policy: PolicyRef, policy_version: i64, max_age: Duration)
        -> Result<Option<LifecycleRun>, StoreError>;
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}
pub enum PolicyRef { Cleanup(CleanupPolicyId, Option<RepositoryId>), Retention(RetentionPolicyId) }

#[async_trait]
pub trait GcRunRepo: Send + Sync {
    async fn create(&self, run: &GcRun) -> Result<(), StoreError>;
    async fn get(&self, id: GcRunId) -> Result<Option<GcRun>, StoreError>;
    async fn list(&self, page: &PageRequest) -> Result<Page<GcRun>, StoreError>;
    async fn update_progress(&self, id: GcRunId, counts: &GcCounts, per_store: &serde_json::Value) -> Result<(), StoreError>;
    async fn finish(&self, id: GcRunId, status: TaskRunStatus, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Sets the flag; the sweep reads it before every batch. Returns `false` for a finished run.
    async fn request_stop(&self, id: GcRunId) -> Result<bool, StoreError>;
    async fn stop_requested(&self, id: GcRunId) -> Result<bool, StoreError>;
    /// Dry run report sample, capped at 1000 rows per run.
    async fn save_samples(&self, samples: &[GcSample]) -> Result<(), StoreError>;
    async fn list_samples(&self, run: GcRunId, page: &PageRequest) -> Result<Page<GcSample>, StoreError>;
    /// Start time of the newest manual run, for the one per minute limit.
    async fn last_manual_start(&self) -> Result<Option<OffsetDateTime>, StoreError>;
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

#[async_trait]
pub trait DownloadStatsRepo: Send + Sync {
    /// Daily counts of the last `days` days (at most 90), oldest first, gaps filled with zero.
    async fn daily(&self, asset: AssetId, days: u32) -> Result<Vec<(Date, u64)>, StoreError>;
}
```

The two settings of this stream: `settings.quota.default_project_bytes` (copied into the `quotas` row of a new project at creation, not applied retroactively) and `settings.storage.last_downloaded_interval_secs` (minimum spacing of `last_downloaded_at` writes per asset, default 3600). Both are in section 8.

### 4.c PostgreSQL

Migrations `0040_download_counters.sql`, `0041_quotas.sql`, `0042_guards.sql`, `0043_policies_and_runs.sql`, `0044_gc_runs.sql`.

```sql
-- 0040: daily download counters. Partitioned by day, one partition per month, dropped by age.
CREATE TABLE asset_downloads_daily (
    asset_id uuid   NOT NULL,
    day      date   NOT NULL,
    count    bigint NOT NULL CHECK (count > 0),
    PRIMARY KEY (asset_id, day)
) PARTITION BY RANGE (day);
-- No foreign key on purpose: a deleted asset's counters age out with their partition, and the flush never contends with
-- deletes. ensure_download_partitions(now, lookahead) and drop_old_download_partitions(retention_days) are written like
-- the audit functions; retention is downloads.retention_days (default 90, at least 35 so the 30 day figure is exact).
ALTER TABLE blob_stores ADD COLUMN soft_quota jsonb CHECK (soft_quota IS NULL OR jsonb_typeof(soft_quota) = 'object');
-- Written by the blob_store_stats job (section 4.d), read by the status checks and by the group adapter (section 7.d).
CREATE TABLE blob_store_stats (
    blob_store_id uuid PRIMARY KEY REFERENCES blob_stores (id) ON DELETE CASCADE,
    used_bytes bigint NOT NULL, blob_count bigint NOT NULL, free_bytes bigint, total_bytes bigint,
    refreshed_at timestamptz NOT NULL
);

-- 0041: quotas. Project limits here; repository limits stay in repositories.quota_bytes.
CREATE TABLE quotas (
    project_id   uuid        PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    limit_bytes  bigint      CHECK (limit_bytes IS NULL OR limit_bytes >= 0),
    warn_percent smallint    NOT NULL DEFAULT 80 CHECK (warn_percent BETWEEN 1 AND 100),
    version      bigint      NOT NULL DEFAULT 1,
    updated_by   uuid,
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE quota_usage (
    scope_kind   text   NOT NULL CHECK (scope_kind IN ('project', 'repository')),
    scope_id     uuid   NOT NULL,
    used_bytes   bigint NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    object_count bigint NOT NULL DEFAULT 0 CHECK (object_count >= 0),
    -- Mirror of the limit, so "check and add" is one conditional UPDATE of one row. Kept in step by triggers on
    -- quotas and on repositories.quota_bytes.
    limit_bytes  bigint,
    warn_percent smallint NOT NULL DEFAULT 80,
    warned       boolean NOT NULL DEFAULT false,         -- edge state of quota.near_threshold
    last_exceeded_event_at timestamptz,                 -- quota.exceeded is emitted at most every 10 minutes per scope
    recounted_at timestamptz,
    PRIMARY KEY (scope_kind, scope_id)
);

-- 0042: guards. SQLSTATEs RX004 immutable, RX005 held, RX007 read only (db `map_err` learns RX004 to RX008).
CREATE TABLE immutability_rules (
    id            uuid PRIMARY KEY,
    project_id    uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    name          text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    item_kind     text        NOT NULL CHECK (item_kind IN ('tag', 'version', 'path')),
    repo_mode     text        NOT NULL DEFAULT 'matching' CHECK (repo_mode IN ('matching', 'excluding')),
    repo_globs    text[]      NOT NULL DEFAULT '{}' CHECK (cardinality(repo_globs) <= 16),
    repo_regexes  text[]      NOT NULL DEFAULT '{}',   -- written by Rust from repo_globs, anchored, bounded form only
    item_mode     text        NOT NULL DEFAULT 'matching' CHECK (item_mode IN ('matching', 'excluding')),
    item_globs    text[]      NOT NULL DEFAULT '{}' CHECK (cardinality(item_globs) <= 16),
    item_regexes  text[]      NOT NULL DEFAULT '{}',
    enabled       boolean     NOT NULL DEFAULT true,
    version       bigint      NOT NULL DEFAULT 1,
    created_by    uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_immutability_scope CHECK ((project_id IS NULL) <> (repository_id IS NULL)),
    CONSTRAINT ck_immutability_matching_nonempty CHECK (
        (item_mode = 'excluding' OR cardinality(item_globs) > 0) AND (repo_mode = 'excluding' OR cardinality(repo_globs) > 0)),
    CONSTRAINT ck_immutability_regex_shape CHECK (
        cardinality(repo_regexes) = cardinality(repo_globs) AND cardinality(item_regexes) = cardinality(item_globs))
);
CREATE INDEX ix_immutability_project ON immutability_rules (project_id, item_kind) WHERE enabled AND project_id IS NOT NULL;
CREATE INDEX ix_immutability_repo    ON immutability_rules (repository_id, item_kind) WHERE enabled AND repository_id IS NOT NULL;
-- At most 15 rules per scope: a BEFORE INSERT trigger counts under the scope's advisory transaction lock.

CREATE TABLE lifecycle_holds (
    id           uuid PRIMARY KEY,
    kind         text        NOT NULL CHECK (kind IN ('legal', 'release', 'promotion')),
    component_id uuid        REFERENCES components (id) ON DELETE CASCADE,
    repository_id uuid       REFERENCES repositories (id) ON DELETE CASCADE,
    project_id   uuid        REFERENCES projects (id) ON DELETE CASCADE,
    reason       text        NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    created_by   uuid,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    released_at  timestamptz,
    released_by  uuid,
    CONSTRAINT ck_holds_one_target CHECK (num_nonnulls(component_id, repository_id, project_id) = 1)
);
CREATE INDEX ix_holds_component  ON lifecycle_holds (component_id)  WHERE released_at IS NULL AND component_id IS NOT NULL;
CREATE INDEX ix_holds_repository ON lifecycle_holds (repository_id) WHERE released_at IS NULL AND repository_id IS NOT NULL;
CREATE INDEX ix_holds_project    ON lifecycle_holds (project_id)    WHERE released_at IS NULL AND project_id IS NOT NULL;
```

The guard functions and triggers of 0042 (written once, used by every table that can lose or change content):

| Object | Behaviour |
|---|---|
| `rx_immutable_rule(repository uuid, project uuid, kind text, item text) RETURNS uuid` | first enabled rule of the repository's scope or its project's scope with that `item_kind` whose repository selector matches the repository name and whose item selector matches `item`. A selector matches when `mode = matching` and the item matches any stored regex, or `mode = excluding` and it matches none |
| `rx_active_legal_hold(repository uuid, project uuid, component uuid) RETURNS uuid` | an active `legal` hold on the component, its repository or its project |
| `assets_guard` (BEFORE UPDATE OF `blob_id` OR DELETE ON `assets`, row level) | skips everything when `current_setting('rustryx.override_immutable', true) = 'on'` for immutability; raises `RX004` when `rx_immutable_rule(.., 'path', OLD.path)` or, for an asset with a component, `rx_immutable_rule(.., 'version', component.version)` returns a rule; raises `RX005` for a legal hold. An UPDATE whose new blob has the **same SHA-256** as the old one is allowed (an idempotent re-push, ADR-W2-10) |
| `components_guard` (BEFORE DELETE ON `components`) | the same two checks for the component version and holds |
| tag guard | the OCI crate's migration attaches `rx_guard_tag_change()` (provided by 0042, same logic with `item_kind = 'tag'`) to its tag table, see 4.g |
| `rx_assert_writable()` and `assets_readonly` / `components_readonly` (BEFORE INSERT OR DELETE OR UPDATE OF `blob_id` ON `assets`; BEFORE INSERT OR DELETE ON `components`; statement level) | raise `RX007` while `system_config` key `system.read_only` has `enabled = true`. One primary key lookup per statement. `last_downloaded_at` and counters are not guarded |

```sql
-- 0043: policies and runs
CREATE TABLE cleanup_policies (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    description text NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    format text CHECK (format IS NULL OR format ~ '^[a-z][a-z0-9_]{0,31}$'),
    criteria jsonb NOT NULL CHECK (jsonb_typeof(criteria) = 'object'),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_cleanup_policies_name UNIQUE (name)
);
CREATE INDEX ix_repositories_cleanup_policies ON repositories USING gin (cleanup_policy_ids);

CREATE TABLE retention_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    rules jsonb NOT NULL CHECK (jsonb_typeof(rules) = 'array' AND jsonb_array_length(rules) BETWEEN 1 AND 15),
    allow_delete_last_version boolean NOT NULL DEFAULT false,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_retention_scope CHECK ((project_id IS NULL) <> (repository_id IS NULL))
);
CREATE UNIQUE INDEX uq_retention_project ON retention_policies (project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_retention_repo    ON retention_policies (repository_id) WHERE repository_id IS NOT NULL;

CREATE TABLE lifecycle_runs (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('cleanup', 'retention', 'label_cleanup')),
    cleanup_policy_id uuid REFERENCES cleanup_policies (id) ON DELETE SET NULL,
    retention_policy_id uuid REFERENCES retention_policies (id) ON DELETE SET NULL,
    repository_id uuid REFERENCES repositories (id) ON DELETE SET NULL,
    dry_run boolean NOT NULL,
    policy_version bigint NOT NULL,
    task_run_id uuid,
    status text NOT NULL,
    counts jsonb NOT NULL DEFAULT '{}'::jsonb,
    truncated boolean NOT NULL DEFAULT false,
    requested_by uuid,
    confirmed_mass_delete boolean NOT NULL DEFAULT false,
    started_at timestamptz, finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ix_lifecycle_runs_policy ON lifecycle_runs (cleanup_policy_id, created_at DESC);
CREATE INDEX ix_lifecycle_runs_retention ON lifecycle_runs (retention_policy_id, created_at DESC);
CREATE TABLE lifecycle_run_items (
    run_id uuid NOT NULL REFERENCES lifecycle_runs (id) ON DELETE CASCADE,
    seq bigint NOT NULL,
    repository text NOT NULL,
    component_id uuid NOT NULL,           -- no foreign key: a deleted component stays in the record
    coordinate text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    size_bytes bigint NOT NULL,
    decision text NOT NULL,
    reason text NOT NULL,
    PRIMARY KEY (run_id, seq)
);
-- Retention of runs and items: 180 days after finish (lifecycle runs are compliance records, longer than task logs).

-- 0044: GC history
CREATE TABLE gc_runs (
    id uuid PRIMARY KEY,
    trigger text NOT NULL CHECK (trigger IN ('schedule', 'manual', 'event', 'startup')),
    options jsonb NOT NULL,
    status text NOT NULL,
    counts jsonb NOT NULL DEFAULT '{}'::jsonb,
    per_store jsonb NOT NULL DEFAULT '{}'::jsonb,
    stop_requested_at timestamptz,
    task_run_id uuid,
    requested_by uuid,
    started_at timestamptz, finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ix_gc_runs_created ON gc_runs (created_at DESC);
CREATE TABLE gc_run_samples (
    run_id uuid NOT NULL REFERENCES gc_runs (id) ON DELETE CASCADE,
    seq integer NOT NULL,
    blob_store text NOT NULL,
    key text NOT NULL,
    size_bytes bigint NOT NULL,
    orphaned_at timestamptz NOT NULL,
    PRIMARY KEY (run_id, seq)
);
-- GC history is kept 90 days.
```

Retention summary: `asset_downloads_daily` 90 days (configurable, at least 35); `lifecycle_runs` and items 180 days; `gc_runs` 90 days; `task_runs` 90 days and `task_run_log` 30 days (foundation); `quota_usage` rows live with their scope.

**Quota accounting in `publish_with`** (the authoritative check, runs on the actual size after the blob is committed to the store, in the same transaction that makes the asset visible):

1. Insert the blob row and upsert the asset (guards fire here: `RX007`, `RX004`, `RX005`).
2. For each scope that has a limit or a counter, in the fixed order repository then project, compute the byte delta: `+size` when no other asset of that scope references this blob (`NOT EXISTS` through `ix_assets_blob`, joined to `repositories` for the project scope), and `- old size` when the replaced blob has no remaining reference in the scope.
3. One statement per scope: `UPDATE quota_usage SET used_bytes = used_bytes + :delta, object_count = ..., ... WHERE scope = :scope AND (:delta <= 0 OR limit_bytes IS NULL OR used_bytes + :delta <= limit_bytes) RETURNING ...`. Zero rows means the limit would be exceeded: the db reads usage and limit, rolls the transaction back (no asset, no blob row, no outbox row) and returns `PolicyViolation::QuotaExceeded`. The row lock is taken last and held only to commit, so busy projects serialize only the short tail of a publish. A project with a very high publish rate can still become a hot row; that limit is accepted and documented (one project, a few hundred commits per second).
4. When a successful update crosses `warn_percent` upward and `warned` is false, set `warned` and insert `quota.near_threshold` (outbox, same transaction). When usage falls below `warn_percent - 5` the delete path clears `warned`. A rejection emits `quota.exceeded` through the caller, at most once per 10 minutes per scope (`last_exceeded_event_at`).
5. The pre check (`QuotaRepo::headroom`) runs before the body is read, compares the declared `Content-Length`, and answers 507 early. It is advisory; an unknown length proceeds and is stopped by step 3 on the actual bytes, and the blob is deleted from the store by the pipeline's existing cleanup.

Counters are authoritative only when the `QuotaCounters` gate is open **and** the first full `quota_recount` finished (`system_config` key `quota.counters_ready`); until then the pre check and the commit check use the phase 1 sum (`BlobRepo::used_bytes_by_repository`), so an N-1 pod that does not maintain counters cannot make them lie.

Expand and contract. 0040 to 0044 are expand only; the `quota_bytes` columns are not contracted (they remain the repository scope source, ADR-W2-11). The guards of 0042 are active in the same release for every binary, N-1 included. Gates: `QuotaCounters` and `ReadOnlyMode` (an N-1 pod ignores the flag in its middleware; the SQL guard still stops its writes with `RX007`, so the gate protects only against confusing errors, not against inconsistency).

### 4.d Jobs

| Job | What it does | Idempotency and safety | Metrics |
|---|---|---|---|
| `blob_gc` (per store, hourly) | **Mark**: select unreferenced blobs of the store whose `orphaned_at` is older than `min_age` (`BlobRepo::list_unreferenced_in`), up to `gc.batch_size` per batch. **Sweep** per blob, through `BlobRepo::delete_if_unreferenced`: one transaction takes the blob row `FOR UPDATE`, re-checks that `orphaned_at` is older than the cutoff and that no asset references it (a reference created since the mark wins and the blob is kept), deletes the **row** and commits; only then the runner deletes the **object** from the store. Abandoned staging is removed by the same run (`BlobStore::list_stale_staging`). `delete_untagged` first deletes untagged OCI components through the lifecycle path. A dry run executes only the mark and stores counts, bytes per store and a sample of 1000 blobs | row first, object second is deliberate and reverses the phase 1 wording of `BlobRepo::delete`: with `AssetRepo::link` an orphaned blob can be referenced again, and the row lock plus the foreign key make that impossible after the commit, whereas deleting the object first could leave an asset that points at nothing (data loss). A crash between the commit and the object delete leaves an orphan object, a leak that the store sweep of `verify_blobs` finds and removes; a stop request is read before every batch and already deleted blobs are not restored; a manual trigger is limited to one per minute; `min_age` can be raised per run, never lowered below `gc.min_age_secs` | `gc_blobs_deleted_total{result}`, `gc_bytes_reclaimed_total`, `gc_run_duration_seconds`, `gc_candidates` (gauge of the last dry run), standard `background_job_*{job="blob_gc"}` |
| `cleanup` (daily 01:00, per repository with policies) | for each attached `CleanupPolicy`: stream `candidates`, evaluate with the AND criteria, then apply the plan in batches of 500 | see the lifecycle rules below | `lifecycle_runs_total{kind,mode,result}`, `lifecycle_items_total{kind,decision}`, `lifecycle_deleted_bytes_total` |
| `retention` (daily 02:00, per policy) | evaluate the policy rules over the scope, plan, apply | same | same |
| `cleanup_labels` | `UPL-11`: components that carry a label older than N days or a name glob; deletes **labels' links**, and components only with `delete_components: true` | same lifecycle record | same |
| `download_flush` (node daemon, 10 s) | folds the in-memory `DashMap<AssetId, (count, last)>` (fed by `ContentEvent::AssetDownloaded`, source `Client` only) into one `record_downloads` call; the final flush runs inside the shutdown deadline | a failed flush keeps the counts for the next one; the map is capped at 1 000 000 keys and new keys beyond it are dropped and counted; a crash loses at most 10 s of counts, which is the accepted precision of a statistic | `download_counter_pending` gauge, `download_counter_flush_total{result}`, `download_counter_dropped_total` |
| `download_partitions` | creates the next two months and drops partitions older than the retention | SQL functions are idempotent and serialize themselves | standard |
| `quota_recount` (weekly and on demand) | `QuotaRepo::recount` for every scope with a counter, `statement_timeout` raised for the statement, rate limited to 10 scopes per second | corrects drift and sets `quota.counters_ready` after the first full pass | `quota_recount_corrections_total` |
| `blob_store_stats` (every 5 min) | per store: used bytes (sum over blobs), blob count, free and total bytes where the store knows them; evaluates `soft_quota`; writes the gauges and the status check inputs. Local free space comes from a safe crate behind a `DiskProbe` trait (`unsafe` is forbidden in this workspace), which also closes the open SRE-34 item | read only | `blob_store_used_bytes{store}`, `blob_store_free_bytes{store}`, `blob_store_total_bytes{store}`, `blob_store_soft_quota_state{store}` (0 ok, 1 over) |
| `task_purge` | deletes finished `task_runs` (90 days), `task_run_log` (30 days), `lifecycle_runs` and items (180 days), `gc_runs` (90 days) | batched deletes | standard |

**Lifecycle run rules** (apply to `cleanup`, `retention` and `cleanup_labels`):

1. **Dry run first.** A real run of a policy is refused (409 `dry_run_required`) unless a dry run of the **same policy version** finished within `lifecycle.dry_run_validity_days` (default 14). Editing the policy changes its version, so the old dry run no longer counts.
2. **Re-evaluation, not replay.** The real run evaluates again against current data and applies in batches of 500; it never replays the stored plan, so it cannot delete something a user published or protected after the dry run.
3. **Protection order** for every selected component: immutable, then hold, then last version. A protected component is recorded with its decision and is not deleted: immutability by `ImmutabilityRepo::covering_rule`, legal and release holds by `LifecycleHoldRepo::active_for`, the last remaining version of a package unless `allow_delete_last_version`. `override_immutable` exists only on a run requested by `nx-admin` and is written to the audit row.
4. **Mass delete brake.** A real run that would delete more than `lifecycle.max_delete_ratio` (default 0.9) of at least `lifecycle.mass_delete_min_candidates` (default 20) evaluated components stops before deleting and finishes `failed` with `mass_delete_unconfirmed`, unless `nx-admin` requested it with `confirm_mass_delete`. A misconfigured policy therefore stops at the first run.
5. **OCI.** Retention evaluates tags and acts on components: a component is deleted only when every one of its tags is selected for deletion and none is kept, or when it has no tag and a covering rule has `include_untagged`. Referrers (signatures, SBOM) follow their subject and are not candidates themselves.
6. **Retention semantics (the OR rule).** For rule r, the covered set `C(r)` is the items of repositories matched by `r.repository` whose name matches `r.item`; `K(r)` is the subset the template keeps. The deleted set is the union of all `C(r)` minus the union of all `K(r)`. An item covered by no rule is not touched. A project proxy cache repository can be offered the recommended 7 day pull window as a template; it is a choice, never forced.
7. **Audit.** A real run writes one audit row per batch with the run id and the count (`LifecycleAction`); the full list of removed coordinates is `lifecycle_run_items`.
8. **Defaults.** `cleanup` runs daily at 01:00 UTC. compat documents two different defaults (hourly and daily at 1 am), so this is a deliberate pick: a cleanup is a heavy statement set and daily is enough for storage control (ADR-W2-8).

**Read only mode** (`OPS-22`). State is the system config key `system.read_only` = `{enabled, reason, set_by, set_at}` with the revision of `system_config` as its version. `PUT /api/v1/system/read-only` (nx-admin) writes it and publishes `InvalidationEvent::Config(revision)`. Enforcement, from the outside in: (1) the HTTP write middleware answers `503` with code `read_only` and `Retry-After: 60` for every method that is not GET, HEAD or OPTIONS on artifact routes and for every mutating native API route except login, token refresh and the read only endpoint itself (OCI uses its `UNAVAILABLE` error code, pub its own error body); (2) the task runner finishes kinds with `mutates_content` as `skipped(read_only)`; (3) the SQL guard `RX007` stops any writer that slipped past, N-1 pods included. A node applies the flag within `settings_refresh` (5 s) or at once on the bus event. **Quiesce check**: `GET /api/v1/system/read-only/quiesced` is true when every live node reports `settings_revision >= flag revision` and `inflight_writes = 0`; that is the signal a backup procedure waits for before snapshotting the database and the blob stores together. A status check goes critical when the mode has been on for more than one hour (`RustryxReadOnlyLong`). The flag never auto-clears.

### 4.e Authorization

| Operation | Requires |
|---|---|
| view quota usage of a project | `Project(ViewQuota)` (all roles); of a repository: `Repo(browse)` |
| set a limit, recount | `App(Quotas, Write)`, phase 1 `nx-admin`; never a project permission |
| cleanup policies (global objects) | `App(Tasks, Write)`; attach to a repository: `Repo(admin)` (phase 1 `nx-admin`) |
| retention policy, immutability rules of a project | `Project(ManagePolicies)` |
| immutability rule or retention policy of a repository without a project | `nx-admin` |
| run now (real), run dry run | `Project(ManagePolicies)` or `App(Tasks, Write)`; `override_immutable` and `confirm_mass_delete`: `nx-admin` only |
| legal hold create or release | `nx-admin`; release and promotion holds are system made |
| GC run, stop, history, task administration, read only mode | `App(Gc, Write)`, `App(Tasks, Write)`, `App(System, Write)`; phase 1 `nx-admin` |
| read lifecycle run reports | `Project(ViewAudit)` for project scoped runs, `App(Tasks, Read)` otherwise |

### 4.f Events and audit

| Event | Outbox (`EventKind`) | Audit `action` and kind | Data |
|---|---|---|---|
| download | `pull` (only while subscribed) | none (counted, not audited) | repository, coordinate or tag, digest |
| delete by a user | `delete` (in the same transaction as the delete) | `asset.delete` (existing) | repository, path or tag |
| immutable reject | none | `immutable.reject`, `SecurityDecision` | repository, item, rule id, operation |
| held reject | none | `hold.reject`, `SecurityDecision` | repository, item, hold id |
| quota reject | `quota.exceeded` (caller, `enqueue`) | `quota.reject`, `SecurityDecision` | scope, limit, used, requested |
| threshold | `quota.near_threshold` (db, same transaction) | none | scope, limit, used, percent |
| policy change | none | `retention.update`, `immutability.update`, `quota.update`, `hold.create`, `hold.release`, `cleanup_policy.update`, `PolicyChange` | policy name and version, before and after summary |
| lifecycle batch | none | `lifecycle.delete_batch`, `LifecycleAction` | run id, count, bytes |
| retention run end | `retention.finished` | `retention.run`, `LifecycleAction` | project, counts, dry run |
| gc run end | `gc.finished` | `gc.run`, `LifecycleAction` | counts, dry run, trigger |
| read only mode | none | `system.read_only`, `SettingsChange` | enabled, reason |
| task run | `task.finished` (when the definition asks) | `task.run`, `LifecycleAction` for manual runs | kind, trigger, status |

### 4.g Crates that change

| Crate | Change |
|---|---|
| `rustryx-core` | `quota.rs`, `lifecycle.rs`, `ports/lifecycle.rs`, `tasks.rs`, `events.rs`, `node.rs`, `ports/system.rs` (parts 1 to 3), `WriteOptions`, `AssetRepo` additions |
| `rustryx-tasks` | everything of section 2.3 plus the engines of this section: `LifecycleEngine` (pure evaluation over `Candidate` streams, unit tested without a database), `GcRunner`, `QuotaRecounter`, `DownloadAggregator`, the handlers of the catalog rows of this stream |
| `rustryx-db` | migrations 0040 to 0044, the guards, `publish_with` and the quota SQL, `record_downloads`, the repositories of 4.b, partition functions, the outbox and task repositories |
| `rustryx-format-core` | `ContentEvent::AssetDownloaded { asset, repository, source, at }` (the download path emits it, `DownloadSource::Client` only); `FormatHandler::lifecycle() -> Option<&dyn LifecycleAdapter>`; the hosted pipeline calls `QuotaRepo::headroom` for the pre check and passes `WriteOptions` (actor, request id, push event) to `publish_with`; proxy fills use `PublishMode::CreateOnly` |
| `rustryx-format-oci` | contract below |
| `rustryx-api` | the write middleware for read only mode, native endpoints of section 4.b, the `Requires` declarations |
| `rustryx-blob` | `BlobStore::list_stale_staging` stays; the free space probe |
| `rustryx-testkit` | `MemStore` implements the guards (immutability by the same glob translation, holds, quota, read only), `MemTaskStore`, `MemOutbox` |

```rust
// crate: rustryx-format-core  (new trait)
pub trait LifecycleAdapter: Send + Sync {
    /// Which `CleanupCriteria` fields the format understands; the rest are refused when a policy is attached.
    fn supported_criteria(&self) -> CriteriaSupport;
    fn release_type(&self, name: &str, version: &str) -> Option<ReleaseType>;
    /// Version order for `keep_latest_versions` (maven, semver, PEP 440, ...). Unparsable versions sort oldest.
    fn compare_versions(&self, a: &str, b: &str) -> std::cmp::Ordering;
    /// What immutability rules and retention rules compare against: OCI `Tag`, packages `Version`, raw `Path`.
    fn item_kind(&self) -> ItemKind;
    /// Marks the upload that completes a component, so one `push` event is emitted per component (section 2.2).
    fn completes_component(&self, request: &ArtifactRequest) -> bool;
    /// Validates `CleanupCriteria::extra` (for example the Maven snapshot template).
    fn validate_extra(&self, extra: &serde_json::Value) -> Result<(), FormatError>;
}
```

**Contract the OCI crate (phase 1, iteration 3) must fulfil**, because retention, immutability, scanning, replication and promotion all depend on it. These are interface requirements, to be reconciled with the OCI agent's own design when that crate lands (open item):

1. One manifest digest is one `Component` (`group_id` none, `name` the image path inside the repository, `version` the digest). Tags are rows of an OCI owned table `oci_tags(repository_id, name, tag, component_id, pushed_at, pushed_by)` with a unique `(repository_id, name, tag)`.
2. The table gets `rx_guard_tag_change()` in the OCI migration (BEFORE UPDATE OF `component_id` OR DELETE), so a tag move or delete honours immutability and holds. A move to the **same digest** is allowed.
3. Port `OciCatalogRepo` (read) and `OciTagRepo` (write, with `WriteOptions`): list tags of a component, list components of an image with push and pull times, tag, untag, list untagged components older than a time, list referrers of a subject digest, create a referrer link, delete a subject with its referrers. Defined in the OCI crate; the lifecycle, scan and replication code uses it through core ports when that crate publishes them.
4. Pull time of a manifest (`components.last_downloaded_at`) is set through `record_downloads` on the manifest asset.

### 4.h Negative requirements

| Requirement | Test |
|---|---|
| An immutability rule beats overwrite, delete, retag, lifecycle delete, promotion move, replication replace and a cascading repository delete | `db::must_not_overwrite_an_immutable_item`, `..._delete_...`, `..._retag_...`, `..._cascade_delete_...`; `tasks::must_not_delete_an_immutable_item_in_cleanup`; `api::must_not_delete_an_immutable_item_by_any_route` |
| An immutable path may be pushed again only with identical content | `db::must_allow_identical_content_republish_and_reject_changed_content` |
| `override_immutable` cannot be set from request data | `api::must_not_accept_override_immutable_from_a_request`, `tasks::must_not_override_without_an_admin_run` |
| The glob to regex translation matches an independent glob matcher and never emits an unbounded construct | `core::glob_translation_matches_reference` (proptest), `core::must_not_emit_nested_quantifiers` |
| Quota is checked before the body is read and again on the actual size in the commit | `api::must_not_read_a_body_over_the_declared_headroom`, `db::must_not_exceed_quota_when_content_length_lies`, `db::must_not_exceed_quota_under_concurrent_publishes` |
| A rejected publish leaves no asset, blob row, counter change or outbox row | `db::must_not_leave_anything_after_a_quota_rejection` |
| Shared blobs count once per scope | `db::must_not_count_a_shared_blob_twice_in_one_project` |
| Reads and deletes continue at 100 percent quota | `api::must_not_block_reads_or_deletes_when_over_quota` |
| Counters are not authoritative before the gate and the first recount | `db::must_not_trust_counters_before_the_first_recount` |
| Read only mode blocks writes through the middleware, the runner and the SQL guard, and does not block reads | `api::must_not_write_in_read_only_mode`, `tasks::must_not_run_mutating_tasks_in_read_only_mode`, `db::must_not_write_assets_in_read_only_mode` |
| A real lifecycle run needs a dry run of the same policy version | `tasks::must_not_run_without_a_dry_run_of_the_same_version` |
| The mass delete brake stops a policy that selects nearly everything | `tasks::must_not_delete_when_the_mass_delete_brake_trips_unconfirmed` |
| A lifecycle run never deletes held, immutable or last-version items | `tasks::must_not_delete_held_items`, `..._the_last_version` |
| A retention rule never touches an item no rule covers | `tasks::must_not_delete_items_no_rule_covers` |
| An OCI component with a kept tag is never deleted | `tasks::must_not_delete_a_component_with_a_kept_tag` |
| Referrers are never deleted on their own, always with their subject | `tasks::must_not_delete_a_referrer_without_its_subject` |
| A deletion record exists even when the audit sink fails | `tasks::must_not_lose_the_deletion_record_when_audit_fails` |
| GC never deletes a blob younger than `min_age`, nor one that gained a reference after the mark | `tasks::must_not_gc_a_fresh_blob`, `..._a_blob_referenced_after_the_mark` |
| GC deletes the row (under its lock) before the object, so an asset can never point at a deleted object | `tasks::must_not_delete_the_object_before_the_row_commit`, `db::must_not_delete_a_blob_that_a_link_references` |
| A manual GC cannot start twice within a minute, and a stop is honoured | `api::must_not_start_gc_twice_within_a_minute`, `tasks::must_stop_gc_between_batches` |
| Scanner and replication reads never count as downloads or refresh pull time | `format_core::must_not_count_scanner_reads`, `..._replication_reads` |
| The flush never double counts a batch it kept after a failure, and respects its map cap | `tasks::must_not_double_count_after_a_failed_flush`, `must_not_grow_the_counter_map_beyond_its_cap` |
| The scheduler fires a due schedule exactly once across nodes, and a singleton kind never runs twice | `tasks::must_not_fire_a_schedule_twice` (two nodes, 1000 ticks), `must_not_run_a_singleton_twice`, `must_not_write_after_lock_lost` |
| A system task cannot be deleted or have its kind changed | `api::must_not_delete_a_system_task` |
| The outbox never skips an event whose transaction commits late | `db::must_not_skip_a_late_committing_event` (concurrent writers against a relay, property test) |

---

## 5. W2-D: supply chain

Capabilities: `SCN-01` to `SCN-29`, `REPO-49`. Decisions of parity sections 11.7, 11.10, 12.1 and of DSO-20 to DSO-28 are made here. The new crate is `rustryx-scan`; its adapters run external tools as separate processes (DSO-22: Trivy is not in the product image) and never build a command line from request data.

Sub-steps: **D1** scanner trait, Trivy adapter, scan queue and worker, reports and findings, summaries; **D2** OSV adapter, allowlists, pull gate, quarantine, Security Hub, export, SBOM, offline data sets; **D3** trust policies and signature verification (after the spikes of section 10).

The scanned subject is a **component** (for OCI one manifest digest; an index is judged by its children, see the gate rules). A scan that cannot run leaves the state "not scanned", never "clean" (DSO-20): absence of a result is a first class state and the gate treats it as such.

### 5.a Entities and enums

```rust
// core: crates/rustryx-core/src/scan.rs  (new file)
/// Ordered. `Negligible` never blocks a pull. `Unknown` is a finding without a usable severity; it counts as `Medium`
/// for the gate (a gate must not be bypassed by a missing score) and is shown as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity { Unknown, Negligible, Low, Medium, High, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind { Vulnerability, Sbom }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScannerKind { Trivy, Osv, Http }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanStatus { Queued, Running, Complete, Failed, Stopped, Unsupported }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScannerAuth {
    None,
    Basic { username: String, password: Secret },
    Bearer { token: Secret },
    ApiKey { header: String, value: Secret },
}

/// A registered scanner. There is no "skip certificate verification" switch: a private CA goes through
/// `ca_certificate_ids` (SEC-26, `AUTH-86` is `out`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScannerRecord {
    pub id: ScannerId,
    pub name: String,
    pub kind: ScannerKind,
    /// `https` URL for `Http`; for `Trivy` and `Osv` always `None` (they are local processes).
    pub endpoint: Option<String>,
    pub auth: ScannerAuth,
    pub ca_certificate_ids: Vec<TrustedCertificateId>,
    pub capabilities: Vec<ScanKind>,
    pub formats: Vec<RepoFormat>,
    /// Exactly one default scanner per scan kind and format family (partial unique index).
    pub is_default: bool,
    pub enabled: bool,
    pub last_probe: Option<ProbeResult>,
    pub version: i64,
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult { pub at: OffsetDateTime, pub ok: bool, pub version: Option<String>, pub message: String }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    /// Findings per severity, index = `Severity as usize`.
    pub by_severity: [u32; 6],
    pub fixable: u32,
    pub total: u32,
    /// Findings removed from the verdict by the allowlist at the time the report was stored (informational).
    pub suppressed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScanReport {
    pub id: ScanReportId,
    pub component_id: ComponentId,
    pub scanner_id: Option<ScannerId>,
    pub scanner_kind: ScannerKind,
    pub scanner_version: String,
    /// Version of the advisory data the scan used (Trivy DB version, OSV snapshot time).
    pub dataset_version: String,
    pub kind: ScanKind,
    pub status: ScanStatus,
    pub trigger: ScanTrigger,
    pub requested_by: Option<UserId>,
    pub queued_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub summary: ScanSummary,
    /// Short, sanitized, at most 500 characters. Never a command line, a path or a scanner stack trace.
    pub error: Option<String>,
    pub is_current: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger { Push, Manual, Schedule, Rescan, Quarantine }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub report_id: ScanReportId,
    /// Normalized upper case: `CVE-2024-1234`, `GHSA-xxxx-xxxx-xxxx`, other OSV ids as published.
    pub vuln_id: String,
    pub package: String,
    pub package_version: String,
    pub fixed_version: Option<String>,
    pub severity: Severity,
    pub cvss: Option<f32>,
    /// At most 200 characters.
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineState { None, Quarantined, Released }

/// One row per component: what the gate, the list views and the Security Hub read without touching findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComponentSecurityState {
    pub component_id: ComponentId,
    pub report_id: Option<ScanReportId>,
    /// `None` is "not scanned".
    pub scan_status: Option<ScanStatus>,
    pub summary: ScanSummary,
    pub scanned_at: Option<OffsetDateTime>,
    pub quarantine: QuarantineState,
    pub released_by: Option<UserId>,
    pub released_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode { Enforce, AuditOnly }

/// A threshold of `None` switches the gate off. `Negligible` is rejected as a threshold (it would block everything).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGatePolicy {
    pub threshold: Option<Severity>,
    pub mode: GateMode,
    /// In `Enforce`: may a component without a complete scan be pulled? Default false (fail closed).
    pub allow_unscanned: bool,
    /// Treat a report older than this many days as not scanned (`None`: never).
    pub max_report_age_days: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistMode { System, Project }

/// The settings of the scanning feature at one scope. A `None` field inherits (repository over project over system); the
/// system row is seeded and always complete: `auto_scan` true, gate `High` and `AuditOnly`, quarantine off, allowlist
/// mode `System`, health check off, SBOM on push off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScanPolicy {
    pub id: SecurityPolicyId,
    pub scope: PolicyScope,
    pub auto_scan: Option<bool>,
    pub scanner_id: Option<ScannerId>,
    pub gate: Option<PullGatePolicy>,
    pub quarantine: Option<bool>,
    pub allowlist_mode: Option<AllowlistMode>,
    pub health_check: Option<bool>,
    pub sbom_on_push: Option<bool>,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum AllowlistScope { System, Project(ProjectId), Component(ComponentId) }

/// A CVE exception with a recorded reason, author and optional expiry (`SCN-12`, `SCN-13`, DSO-24). An expired entry
/// stops counting at once; the expiry itself is audited by `allowlist_expiry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CveAllowlistEntry {
    pub id: CveAllowlistEntryId,
    pub scope: AllowlistScope,
    pub vuln_id: String,
    /// Required, 1 to 500 characters.
    pub reason: String,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateDecision {
    Allow,
    /// `AuditOnly` mode: the pull proceeds, the audit row and the event are written.
    AllowAudited { reason: GateReason },
    Deny { reason: GateReason },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum GateReason {
    AboveThreshold { highest: Severity, count: u32, threshold: Severity },
    NotScanned, ScanFailed, ScanStale, Quarantined,
    SignatureMissing, SignatureInvalid,
    /// The verdict could not be computed (store down). Mapped to 503, never to an allow in `Enforce`.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredSignatures { None, Cosign, Notation, Both }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotationLevel { Strict, Permissive, Audit }   // `skip` is refused: it is the presence-only check

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosignIdentity { pub issuer: String, pub subject: String, pub issuer_is_pattern: bool, pub subject_is_pattern: bool }

/// A trust policy. A policy that only checks that a signature exists is not representable: every accepted signature has
/// to verify against the material listed here (ADR-W2-20).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustPolicy {
    pub id: TrustPolicyId,
    pub scope: PolicyScope,
    pub required: RequiredSignatures,
    pub mode: GateMode,
    pub cosign_keys: Vec<TrustMaterialId>,
    pub cosign_roots: Vec<TrustMaterialId>,
    pub cosign_identities: Vec<CosignIdentity>,
    pub cosign_require_transparency_log: bool,
    pub notation_stores: Vec<TrustMaterialId>,
    pub notation_identities: Vec<String>,
    pub notation_level: NotationLevel,
    /// For an index: also verify every child image. Default true.
    pub verify_children: bool,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustMaterialKind { CosignPublicKey, CosignRoot, NotationCa }

/// Public material only. A PEM that contains a private key block is refused at upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustMaterial {
    pub id: TrustMaterialId,
    pub kind: TrustMaterialKind,
    pub name: String,
    pub pem: String,
    pub fingerprint_sha256: String,
    pub not_after: Option<OffsetDateTime>,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
}

/// A downloaded advisory data set (Trivy database, OSV ecosystem dump), stored as a blob so every stateless node can sync it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScannerDataSet {
    pub id: ScannerDataSetId,
    pub scanner_kind: ScannerKind,
    /// `trivy-db`, `trivy-java-db`, `osv-<ecosystem>`.
    pub dataset: String,
    pub version: String,
    pub blob_id: BlobId,
    pub sha256: String,
    pub source: String,
    pub fetched_at: OffsetDateTime,
    pub is_current: bool,
}
```

Gate evaluation is a pure function so it is unit testable without a database:

```rust
// crate: rustryx-scan
pub struct GateInput<'a> {
    pub state: Option<&'a ComponentSecurityState>,
    /// Findings that survive the allowlist, by severity (computed by the caller from the report and the effective allowlist).
    pub effective: &'a ScanSummary,
    pub policy: &'a PullGatePolicy,
    pub now: OffsetDateTime,
}
pub fn evaluate(input: &GateInput<'_>) -> GateDecision;
```

Rules of `evaluate`: no threshold: `Allow`. Quarantined: `Deny(Quarantined)` (a quarantine ignores the mode: release is the only way out). No state, or a status other than `Complete`: `Deny(NotScanned|ScanFailed)` unless `allow_unscanned`. A report older than `max_report_age_days`: `ScanStale`, treated like not scanned. Otherwise the highest effective severity at or above the threshold (`Unknown` counted as `Medium`, `Negligible` never) gives `Deny(AboveThreshold)`. In `AuditOnly` mode every `Deny` becomes `AllowAudited`, except `Quarantined`.

### 5.b Ports

```rust
// core: crates/rustryx-core/src/ports/scans.rs  (new file)
#[async_trait]
pub trait ScannerRepo: Send + Sync {
    async fn create(&self, scanner: &ScannerRecord) -> Result<(), StoreError>;
    async fn get(&self, id: ScannerId) -> Result<Option<ScannerRecord>, StoreError>;
    async fn list(&self) -> Result<Vec<ScannerRecord>, StoreError>;
    async fn update(&self, scanner: &ScannerRecord, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a scan policy references it.
    async fn delete(&self, id: ScannerId) -> Result<(), StoreError>;
    async fn set_default(&self, id: ScannerId) -> Result<(), StoreError>;
    async fn record_probe(&self, id: ScannerId, probe: &ProbeResult) -> Result<(), StoreError>;
}

/// Reports, findings, state and the verdict inputs.
#[async_trait]
pub trait ScanRepo: Send + Sync {
    /// Creates a queued report row (the queue entry itself is a `JobLeaseRepo` job with the dedupe key `scan:<component>:<kind>`).
    async fn start(&self, report: &ScanReport) -> Result<(), StoreError>;
    async fn mark_running(&self, id: ScanReportId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// One transaction: stores the report as `Complete`, deletes the findings of the previous current report, inserts the
    /// new findings (bounded at 50 000 per report; more is `Invalid` and the scan is `Failed` with `too_many_findings`),
    /// flips `is_current`, upserts `component_security_state`, keeps the newest 3 reports per `(component, kind)`, and
    /// writes the `scan.completed` outbox event. A new report therefore replaces the old verdict atomically.
    async fn complete(&self, id: ScanReportId, findings: &[Finding], summary: &ScanSummary,
                      scanner_version: &str, dataset_version: &str, at: OffsetDateTime,
                      events: &[EventDraft]) -> Result<(), StoreError>;
    /// Terminal failure or stop. The previous current report and state are untouched, except the status shown.
    async fn fail(&self, id: ScanReportId, status: ScanStatus, error: &str, at: OffsetDateTime,
                  events: &[EventDraft]) -> Result<(), StoreError>;
    async fn get_report(&self, id: ScanReportId) -> Result<Option<ScanReport>, StoreError>;
    async fn state(&self, component: ComponentId) -> Result<Option<ComponentSecurityState>, StoreError>;
    async fn states(&self, components: &[ComponentId]) -> Result<Vec<ComponentSecurityState>, StoreError>;
    async fn findings(&self, component: ComponentId, filter: &FindingFilter, page: &PageRequest) -> Result<Page<Finding>, StoreError>;
    /// Counts by severity of the current report's findings that match none of `allowed` (the verdict inputs).
    async fn effective_summary(&self, component: ComponentId, allowed: &[String]) -> Result<ScanSummary, StoreError>;
    /// Components that were never scanned, or whose current report is older than `before`, newest first, for the rescan job.
    async fn stale_components(&self, before: OffsetDateTime, scope: Option<PolicyScope>, limit: u32) -> Result<Vec<ComponentId>, StoreError>;
    /// Quarantine transitions, with the outbox event in the same transaction.
    async fn set_quarantine(&self, component: ComponentId, state: QuarantineState, by: Option<UserId>,
                            at: OffsetDateTime, events: &[EventDraft]) -> Result<(), StoreError>;
}
#[derive(Debug, Clone, Default)]
pub struct FindingFilter { pub severity_at_least: Option<Severity>, pub fixable: Option<bool>, pub vuln_id: Option<String>, pub package: Option<String> }

#[async_trait]
pub trait CveAllowlistRepo: Send + Sync {
    async fn create(&self, entry: &CveAllowlistEntry) -> Result<(), StoreError>;     // Conflict on the same vuln in the same scope
    async fn list(&self, scope: AllowlistScope, include_expired: bool, page: &PageRequest) -> Result<Page<CveAllowlistEntry>, StoreError>;
    async fn update(&self, entry: &CveAllowlistEntry, expected_version: i64) -> Result<(), StoreError>;
    async fn delete(&self, id: CveAllowlistEntryId) -> Result<(), StoreError>;
    /// Copies the system list into a project scope (a snapshot, it does not track later system removals).
    async fn copy_system_to_project(&self, project: ProjectId, by: Option<UserId>) -> Result<u64, StoreError>;
    /// Ids that are active now for a component: its own, its project's when the mode is `Project`, else the system's;
    /// plus the component's. Used to build `effective_summary`.
    async fn active_ids(&self, component: ComponentId, mode: AllowlistMode) -> Result<Vec<String>, StoreError>;
    /// Entries that expired and were not yet audited, marked in the same call. For `allowlist_expiry`.
    async fn take_expired_unaudited(&self, now: OffsetDateTime, limit: u32) -> Result<Vec<CveAllowlistEntry>, StoreError>;
}

#[async_trait]
pub trait SecurityPolicyRepo: Send + Sync {
    async fn upsert_scan_policy(&self, policy: &ScanPolicy, expected_version: Option<i64>) -> Result<(), StoreError>;
    async fn get_scan_policy(&self, scope: PolicyScope) -> Result<Option<ScanPolicy>, StoreError>;
    /// Field-wise merge repository over project over system for one repository. Cacheable per `security.revision`.
    async fn effective_for(&self, repository: RepositoryId) -> Result<ScanPolicy, StoreError>;
    async fn upsert_trust_policy(&self, policy: &TrustPolicy, expected_version: Option<i64>) -> Result<(), StoreError>;
    async fn get_trust_policy(&self, scope: PolicyScope) -> Result<Option<TrustPolicy>, StoreError>;
    async fn effective_trust_for(&self, repository: RepositoryId) -> Result<Option<TrustPolicy>, StoreError>;
}

#[async_trait]
pub trait TrustRepo: Send + Sync {
    async fn create(&self, material: &TrustMaterial) -> Result<(), StoreError>;
    async fn get(&self, id: TrustMaterialId) -> Result<Option<TrustMaterial>, StoreError>;
    async fn list(&self, kind: Option<TrustMaterialKind>) -> Result<Vec<TrustMaterial>, StoreError>;
    /// `Conflict` while a trust policy lists it.
    async fn delete(&self, id: TrustMaterialId) -> Result<(), StoreError>;
}

/// Security Hub reads. Totals and the two top lists come from `security_stats` (refreshed by `hub_stats`), search from `findings`.
#[async_trait]
pub trait SecurityHubRepo: Send + Sync {
    async fn stats(&self) -> Result<Option<SecurityStats>, StoreError>;
    /// A CVE id is mandatory (the index is on it, and an unfiltered scan of all findings is refused with `Invalid`).
    async fn search(&self, vuln_id: &str, filter: &HubFilter, page: &PageRequest) -> Result<Page<HubRow>, StoreError>;
    async fn refresh_stats(&self, now: OffsetDateTime) -> Result<(), StoreError>;
}
pub struct SecurityStats { pub computed_at: OffsetDateTime, pub components_by_severity: [u64; 6], pub findings_by_severity: [u64; 6],
                           pub top_components: Vec<(ComponentId, u32)>, pub top_vulns: Vec<(String, u32)> }
#[derive(Debug, Clone, Default)]
pub struct HubFilter { pub severity: Option<Severity>, pub min_cvss: Option<f32>, pub max_cvss: Option<f32>, pub project: Option<ProjectId>,
                       pub repository: Option<RepositoryId>, pub digest: Option<String>, pub package: Option<String>, pub tag: Option<String> }
pub struct HubRow { pub vuln_id: String, pub severity: Severity, pub cvss: Option<f32>, pub package: String, pub package_version: String,
                    pub fixed_version: Option<String>, pub component_id: ComponentId, pub repository: String, pub coordinate: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExportJob { pub id: ExportId, pub kind: String, pub requested_by: Option<UserId>, pub params: serde_json::Value,
                       pub status: TaskRunStatus, pub blob_id: Option<BlobId>, pub expires_at: OffsetDateTime,
                       pub downloaded_at: Option<OffsetDateTime>, pub created_at: OffsetDateTime }

#[async_trait]
pub trait ExportRepo: Send + Sync {
    async fn create(&self, job: &ExportJob) -> Result<(), StoreError>;
    async fn get(&self, id: ExportId) -> Result<Option<ExportJob>, StoreError>;
    async fn finish(&self, id: ExportId, status: TaskRunStatus, blob: Option<BlobId>) -> Result<(), StoreError>;
    /// Marks the one allowed download. `false` when it was already downloaded or expired.
    async fn claim_download(&self, id: ExportId, by: Option<UserId>, at: OffsetDateTime) -> Result<bool, StoreError>;
    /// Expired or downloaded jobs; the caller deletes their blobs and calls `delete`.
    async fn due_for_purge(&self, now: OffsetDateTime, limit: u32) -> Result<Vec<ExportJob>, StoreError>;
    async fn delete(&self, id: ExportId) -> Result<(), StoreError>;
}

#[async_trait]
pub trait ScannerDataRepo: Send + Sync {
    async fn current(&self, scanner_kind: ScannerKind, dataset: &str) -> Result<Option<ScannerDataSet>, StoreError>;
    /// Makes this version current and the previous one not current in one transaction.
    async fn publish(&self, set: &ScannerDataSet) -> Result<(), StoreError>;
    /// Versions older than the newest two, to delete with their blobs.
    async fn prunable(&self, scanner_kind: ScannerKind, dataset: &str) -> Result<Vec<ScannerDataSet>, StoreError>;
    async fn delete(&self, id: ScannerDataSetId) -> Result<(), StoreError>;
    /// Short lived scan access tokens for external image scanners.
    async fn create_scan_token(&self, token_hash: &str, scan: ScanReportId, repository: RepositoryId, expires_at: OffsetDateTime) -> Result<(), StoreError>;
    async fn find_scan_token(&self, token_hash: &str, now: OffsetDateTime) -> Result<Option<(ScanReportId, RepositoryId)>, StoreError>;
}
```

The scanner contract lives in the scan crate because it carries a cancellation token:

```rust
// crate: rustryx-scan
#[async_trait]
pub trait Scanner: Send + Sync {
    fn descriptor(&self) -> &ScannerDescriptor;          // kind, capabilities, formats, datasets it needs
    /// Runs one scan. Cancellation (`cx.cancel`) kills the child process and removes the work directory. The deadline is
    /// `scan.timeout_secs`. A scanner that cannot judge the subject returns `ScanError::Unsupported`, which is a state, not clean.
    async fn scan(&self, request: &ScanRequest, cx: &ScanContext) -> Result<ScanOutput, ScanError>;
    /// Cheap reachability and version check, run by `scanner_probe` and at startup (DSO-22).
    async fn probe(&self) -> Result<ProbeResult, ScanError>;
}
pub enum ScanSubject {
    /// Package ecosystems: the purls are built by Rustryx from stored component metadata. No artifact is downloaded.
    Packages { purls: Vec<String> },
    /// An OCI manifest in a repository of this node.
    OciManifest { repository: RepositoryId, digest: String },
}
pub struct ScanRequest { pub component: ComponentId, pub subject: ScanSubject, pub kind: ScanKind, pub sbom_format: SbomFormat }
pub struct ScanOutput { pub findings: Vec<Finding>, pub sbom: Option<SbomDocument>, pub scanner_version: String, pub dataset_version: String }
pub enum SbomFormat { CycloneDxJson, SpdxJson }
#[non_exhaustive]
pub enum ScanError { Unsupported, Timeout, Cancelled, ScannerUnavailable, ScannerFailed { code: i32 }, InvalidOutput, TooLarge, Internal }
```

The pull gate and the signature policy are called by format-core through two small seams that default to "allow", so a deployment without `rustryx-scan` compiles and behaves as phase 1:

```rust
// crate: rustryx-format-core
#[async_trait]
pub trait PullGate: Send + Sync {
    /// After authorization, before any body is read or any proxy fetch starts, for reads of gateable content (OCI
    /// manifests and tag resolution; the primary file of a package component). Scanner and replication-internal reads
    /// that carry `AuthMethod::ScanToken` skip the gate.
    async fn check(&self, repository: &Repository, subject: &GateSubject, principal: &Principal) -> GateOutcome;
}
pub enum GateOutcome { Allow, Deny(GateDenial) }
pub struct GateDenial { pub status: u16, pub code: &'static str, pub message: String, pub retry_after: Option<Duration> }
// status 403 for a policy refusal (OCI renders DENIED per DSO-23), 503 with Retry-After when the verdict could not be computed.
pub struct AllowAllGate;
// FormatHandler gets a provided `fn gate_subject(&self, request: &ArtifactRequest) -> Option<GateSubject> { None }`.
```

### 5.c PostgreSQL

Migrations `0060_scanners.sql`, `0061_reports_findings.sql`, `0062_policies.sql`, `0063_trust.sql`, `0064_datasets_exports.sql`.

```sql
CREATE TABLE scanners (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    kind text NOT NULL CHECK (kind IN ('trivy', 'osv', 'http')),
    endpoint text CHECK (endpoint IS NULL OR endpoint ~ '^https://'),
    auth jsonb NOT NULL DEFAULT '{"type":"none"}'::jsonb,        -- secrets inside are sealed envelopes (purpose scanner.credential)
    ca_certificate_ids uuid[] NOT NULL DEFAULT '{}',
    capabilities text[] NOT NULL,
    formats text[] NOT NULL DEFAULT '{}',
    is_default boolean NOT NULL DEFAULT false,
    enabled boolean NOT NULL DEFAULT true,
    last_probe jsonb,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_scanners_name UNIQUE (name),
    CONSTRAINT ck_scanners_http_endpoint CHECK ((kind = 'http') = (endpoint IS NOT NULL))
);
CREATE UNIQUE INDEX uq_scanners_default_vuln ON scanners (is_default) WHERE is_default AND 'vulnerability' = ANY (capabilities);

CREATE TABLE scan_reports (
    id uuid PRIMARY KEY,
    component_id uuid NOT NULL REFERENCES components (id) ON DELETE CASCADE,
    scanner_id uuid REFERENCES scanners (id) ON DELETE SET NULL,
    scanner_kind text NOT NULL, scanner_version text NOT NULL DEFAULT '', dataset_version text NOT NULL DEFAULT '',
    kind text NOT NULL CHECK (kind IN ('vulnerability', 'sbom')),
    status text NOT NULL CHECK (status IN ('queued', 'running', 'complete', 'failed', 'stopped', 'unsupported')),
    trigger text NOT NULL,
    requested_by uuid,
    queued_at timestamptz NOT NULL DEFAULT now(), started_at timestamptz, finished_at timestamptz,
    summary jsonb NOT NULL DEFAULT '{}'::jsonb,
    error text CHECK (error IS NULL OR length(error) <= 500),
    is_current boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX uq_scan_reports_current ON scan_reports (component_id, kind) WHERE is_current;
CREATE INDEX ix_scan_reports_component ON scan_reports (component_id, queued_at DESC);

-- Findings exist only for the CURRENT report of a component: a new report deletes the old ones in the same transaction,
-- so the table holds one scan's worth per component and no history. Growth is bounded by components x findings (50 000 cap).
CREATE TABLE findings (
    report_id uuid NOT NULL REFERENCES scan_reports (id) ON DELETE CASCADE,
    vuln_id text NOT NULL CHECK (vuln_id <> '' AND length(vuln_id) <= 64),
    package text NOT NULL, package_version text NOT NULL,
    fixed_version text, severity smallint NOT NULL CHECK (severity BETWEEN 0 AND 5), cvss real,
    title text CHECK (title IS NULL OR length(title) <= 200),
    PRIMARY KEY (report_id, vuln_id, package, package_version)
);
CREATE INDEX ix_findings_vuln ON findings (vuln_id, report_id);       -- the Security Hub search (CVE id mandatory)

CREATE TABLE component_security_state (
    component_id uuid PRIMARY KEY REFERENCES components (id) ON DELETE CASCADE,
    report_id uuid REFERENCES scan_reports (id) ON DELETE SET NULL,
    scan_status text, summary jsonb NOT NULL DEFAULT '{}'::jsonb, scanned_at timestamptz,
    quarantine text NOT NULL DEFAULT 'none' CHECK (quarantine IN ('none', 'quarantined', 'released')),
    released_by uuid, released_at timestamptz,
    highest_severity smallint NOT NULL DEFAULT 0       -- denormalized for list sorting and the stale query
);
CREATE INDEX ix_security_state_severity ON component_security_state (highest_severity DESC) WHERE scan_status = 'complete';
CREATE INDEX ix_security_state_scanned ON component_security_state (scanned_at);

-- 0062
CREATE TABLE scan_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    auto_scan boolean, scanner_id uuid REFERENCES scanners (id) ON DELETE SET NULL,
    gate jsonb, quarantine boolean, allowlist_mode text CHECK (allowlist_mode IN ('system', 'project')),
    health_check boolean, sbom_on_push boolean,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_scan_policy_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);
CREATE UNIQUE INDEX uq_scan_policy_system ON scan_policies (system) WHERE system;
CREATE UNIQUE INDEX uq_scan_policy_project ON scan_policies (project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_scan_policy_repo ON scan_policies (repository_id) WHERE repository_id IS NOT NULL;

CREATE TABLE cve_allowlist (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    component_id uuid REFERENCES components (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    vuln_id text NOT NULL CHECK (vuln_id = upper(vuln_id) AND length(vuln_id) <= 64),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    created_by uuid, created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz, expiry_audited_at timestamptz,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_allowlist_scope CHECK (num_nonnulls(project_id, component_id) + (system::int) = 1)
);
CREATE UNIQUE INDEX uq_allowlist_system ON cve_allowlist (vuln_id) WHERE system;
CREATE UNIQUE INDEX uq_allowlist_project ON cve_allowlist (project_id, vuln_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_allowlist_component ON cve_allowlist (component_id, vuln_id) WHERE component_id IS NOT NULL;
CREATE INDEX ix_allowlist_expiry ON cve_allowlist (expires_at) WHERE expires_at IS NOT NULL AND expiry_audited_at IS NULL;
-- Every write to scan_policies, cve_allowlist, trust_policies and trust_materials, and every quarantine change that is not
-- a scan result, bumps system_config key 'security.revision' in the same transaction and sends InvalidationEvent::Config.

-- 0063
CREATE TABLE trust_materials (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('cosign_public_key', 'cosign_root', 'notation_ca')),
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    pem text NOT NULL CHECK (length(pem) <= 65536 AND pem NOT LIKE '%PRIVATE KEY%'),
    fingerprint_sha256 text NOT NULL CHECK (fingerprint_sha256 ~ '^[0-9a-f]{64}$'),
    not_after timestamptz, created_by uuid, created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_trust_materials_name UNIQUE (name), CONSTRAINT uq_trust_materials_fp UNIQUE (kind, fingerprint_sha256)
);
CREATE TABLE trust_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    required text NOT NULL CHECK (required IN ('none', 'cosign', 'notation', 'both')),
    mode text NOT NULL CHECK (mode IN ('enforce', 'audit_only')),
    cosign jsonb NOT NULL DEFAULT '{}'::jsonb, notation jsonb NOT NULL DEFAULT '{}'::jsonb,
    verify_children boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_trust_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);
-- Unique per scope as for scan_policies. Referential integrity of the material ids inside the jsonb columns is checked by
-- the repository in the same transaction (a trigger rejects deleting a trust_materials row that a policy lists).

-- 0064
CREATE TABLE scanner_datasets (
    id uuid PRIMARY KEY,
    scanner_kind text NOT NULL, dataset text NOT NULL, version text NOT NULL,
    blob_id uuid NOT NULL REFERENCES blobs (id) ON DELETE RESTRICT,
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'), source text NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now(), is_current boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX uq_datasets_current ON scanner_datasets (scanner_kind, dataset) WHERE is_current;
CREATE TABLE scan_access_tokens (
    token_hash text PRIMARY KEY CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    scan_id uuid NOT NULL, repository_id uuid NOT NULL, expires_at timestamptz NOT NULL
);
CREATE INDEX ix_scan_tokens_expiry ON scan_access_tokens (expires_at);
CREATE TABLE exports (
    id uuid PRIMARY KEY, kind text NOT NULL, requested_by uuid, params jsonb NOT NULL, status text NOT NULL,
    blob_id uuid REFERENCES blobs (id) ON DELETE SET NULL,
    expires_at timestamptz NOT NULL, downloaded_at timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE security_stats (id boolean PRIMARY KEY DEFAULT true CHECK (id), computed_at timestamptz NOT NULL,
                             data jsonb NOT NULL);
```

Retention: `scan_reports` keeps the newest 3 per `(component, kind)` (the replace transaction deletes older ones; their findings were already deleted), `findings` only for current reports, `scan_access_tokens` rows until one hour after `expires_at`, `exports` 24 hours (the blob deleted with the row), `security_stats` is one row. Growth sizing for operators: findings are the only large table, roughly components times the average findings per component; the 50 000 cap per report bounds a pathological component.

Expand and contract. All five migrations are expand only. The two enforcement gates of section 2.4 apply: `PullGateEnforce` (an N-1 pod serves content ungated, so `mode = enforce` cannot be saved until every node knows the gate; `audit_only` can be saved at once and N-1 simply ignores it) and `Quarantine` (an N-1 proxy would serve a quarantined component). The system scan policy row is created by the startup seed of the first new node, with `audit_only`; no migration inserts a value an older binary has to interpret. No contract step exists for this stream.

### 5.d Jobs

| Job | What it does | Idempotency and safety | Metrics |
|---|---|---|---|
| `scan_worker` (node daemon) | claims queue `scan` from `job_leases` (lease 10 min, heartbeat 30 s), takes a permit of `scan.max_concurrent` (default 2) before it starts any scan, runs the adapter under `scan.timeout_secs` (default 600), then `ScanRepo::complete` or `fail` | the dedupe key `scan:<component>:<kind>` makes a second enqueue a no-op while one is queued or running; a worker that lost its lease stops and writes nothing; a scan never blocks or delays the push that triggered it | `scan_runs_total{scanner,kind,result}`, `scan_duration_seconds{scanner}`, `scan_queue_depth` (gauge, sampled), `scan_findings_total{severity}` |
| outbox consumer `scan` | for each `push` event whose effective policy has `auto_scan` and (for proxies with `health_check`) cached packages: enqueue a scan with `JobLeaseRepo::enqueue_batch` and the cursor advance in one transaction. For a repository with `quarantine`, the component was already marked `quarantined` by the proxy fill, and the job enqueues at priority (separate queue `scan_priority`, claimed first) | exactly once into the queue, at least once delivered | `outbox_lag_seconds{consumer}` |
| `scan_all` | enqueues every component of the scope (system, project or repository) that matches `scan_all` parameters; rate limited to `scan.enqueue_per_second` (default 200) so a large registry does not flood the queue | dedupe keys make a repeat harmless; "stop scan all" sets `stop_requested` and the task ends after the current batch, queued scans are removed by `JobLeaseRepo` purge of that dedupe prefix | standard |
| `scan_rescan_stale` (daily 05:00) | re-evaluates stored data against updated advisories **without reading bytes**: for package ecosystems it re-runs the OSV matcher over the stored purls, for OCI it re-queues images whose report predates the current Trivy data set by more than `scan.rescan_after_days` (default 7); also re-queues components whose last scan failed | the replace is atomic; a rescan that produces the same result still updates `scanned_at` | `scan_runs_total{trigger="rescan"}` via the label of the run |
| `scanner_dataset_update` (every 6 h, per data set) | downloads the data set from the configured source through `Purpose::Scanner` egress (a registry for the Trivy DB, an internal mirror URL for OSV dumps), verifies size and SHA-256, writes it to the blob store, `ScannerDataRepo::publish`, prunes older than the newest two. Each node's `settings_refresh`-like sync loop (`scan.dataset_sync_secs`, default 300) copies the current version into its local `scan.work_dir` and swaps it in atomically | offline sites feed the same blob from a file import (`rustryx scanner import`); online fetching is off unless a source is configured | `scanner_db_age_seconds{scanner}` (age of the current data set), `scanner_dataset_updates_total{result}` |
| `scanner_probe` (node daemon, 5 min, and once at startup) | `Scanner::probe` for every enabled scanner, records the result on the record and the metric. A scanner that is down makes new scans `failed`, not clean | read only | `scanner_up{scanner}` (0 or 1, scanner kind label) |
| `hub_stats` (every 5 min) | `SecurityHubRepo::refresh_stats`: totals by severity, the top 5 components and top 5 CVEs, written to the single `security_stats` row | idempotent | standard |
| `export_purge` (hourly) | deletes expired or downloaded exports and their blobs | idempotent | standard |
| `allowlist_expiry` (daily 00:20) | `take_expired_unaudited`: one audit row per entry that expired, then `expiry_audited_at` is set in the same call | the mark makes it once only | standard |

**Scan on publish needs a trigger for proxies too.** A proxy fill that creates a new component in a repository whose effective policy has `quarantine` writes `component_security_state.quarantine = 'quarantined'` in the same transaction as the publish (through `WriteOptions`), so there is no window in which the component is readable and unscanned. The request that caused the fill receives the quarantine answer too (403 with reason `quarantined`, DSO-25), not the bytes.

**Offline use (`SCN-20`).** With `scan.osv_mode = offline` (the default) and Trivy run with its network features switched off, no scan or rescan contacts a third party. Online OSV (`online`) sends package coordinates to a third party, so it is off by default and a status check names it when on (ADR-W2-17).

### 5.e Authorization

| Operation | Requires |
|---|---|
| view a component's scan summary, findings, SBOM, accessories | `Repo(browse)`, and for project repositories `Project(ViewFindings)` for the findings detail (limited guests see the summary severity only) |
| trigger a scan, an SBOM, stop a scan | `Project(RunScans)`; for a repository without a project `Repo(write)` |
| scan all, schedule scan all | `App(Scanners, Write)`, phase 1 `nx-admin`; per project scope `Project(ManagePolicies)` |
| scanners, system CVE allowlist, system trust policy, trust material, datasets | `App(Scanners, Write)`, phase 1 `nx-admin`. Never a project permission |
| project scan policy, gate, quarantine, allowlist mode, project allowlist, component exception, project trust policy | `Project(ManagePolicies)` |
| release a quarantined component | `Project(ManagePolicies)`, or `nx-admin` for a repository without a project; audited with the reason |
| Security Hub (all projects) | `App(Scanners, Read)`, phase 1 `nx-admin` (admin only, as in the reference product); project scoped views use `ViewFindings` on that project |
| exports | the requester only, and only for projects they have `ViewFindings` on; admin for all |

Scan tokens and scanner reads: `AuthMethod::ScanToken` principals are authenticated by `scan_access_tokens` (SHA-256, 15 minute lifetime, bound to one repository), have the single grant `read` on that repository (rbac rule 4), and bypass the pull gate and the download counters. They cannot reach any other route. The pull gate does apply to every other principal, replication included (a replication rule should not copy what the gate would refuse).

### 5.f Events and audit

| Event | Outbox | Audit `action` and kind | Data |
|---|---|---|---|
| scan finished | `scan.completed` (same transaction as `complete`), `scan.failed`, `scan.stopped` | none for success; `scan.failed` is a log and a metric | component, scanner kind, summary counts, report id, scan kind |
| pull blocked | `pull.blocked` (caller, `enqueue`, deduplicated per component and principal per minute) | `pull.blocked`, `SecurityDecision` | repository, coordinate or digest, threshold, highest severity, count, mode, principal |
| audit only decision | none | `pull.would_block`, `SecurityDecision` | same, with `mode = audit_only` |
| quarantine | `quarantine.entered` (in the publish transaction), `quarantine.released` | `quarantine.release`, `SecurityDecision` | component, releaser, reason |
| allowlist | none | `allowlist.create`, `.update`, `.delete`, `.expire`, `PolicyChange` | scope, vuln id, reason, expiry, author |
| policies | none | `scan_policy.update`, `trust_policy.update`, `trust_material.create`, `.delete`, `PolicyChange` | scope, before and after summary |
| signature decision | none | `signature.reject`, `SecurityDecision` | repository, digest, scheme, reason code (never certificate text) |
| scanner config | none | `scanner.create`, `.update`, `.delete`, `.probe_failed`, `IntegrationChange` | name, kind, endpoint host (no secrets) |

### 5.g Crates that change, configuration

| Crate | Change |
|---|---|
| `rustryx-core` | `scan.rs`, `ports/scans.rs`, `AuthMethod::ScanToken`, `JobLeaseRepo` additions |
| `rustryx-scan` (new) | `Scanner` trait and adapters (`trivy`, `osv`, `http`), the subprocess runner (no shell, fixed argv, cleared environment, per scan work directory removed on every path, output size cap), scan worker and queue producer, `evaluate`, `DbPullGate`, `SignatureVerifier` and the trust policy logic, SBOM parsing and display with size caps (a 64 MiB parse limit, streaming JSON on the blocking pool with a permit), hub stats, export writer, dataset sync |
| `rustryx-db` | migrations 0060 to 0064, repositories, the `security.revision` bump, `effective_for` merge SQL |
| `rustryx-format-core` | `PullGate` seam in the engine (after authorization, before the handler), `GateSubject`, `FormatHandler::gate_subject`, quarantine write in the proxy fill, `ScanToken` handling in the principal checks |
| `rustryx-format-oci` | gate the manifest read; accessories and referrers API used by SBOM and signatures (contract of section 4.g); `GET /v2/<name>/referrers/<digest>` is phase 1 |
| `rustryx-auth` | `ScanTokenAuthenticator` (second lookup, like robots) |
| `rustryx-rbac` | scan token and robot rules (section 3.e) |
| `rustryx-egress` | `Purpose::Scanner` |
| `rustryx-api` | `/api/v1/scanners`, `/scans`, `/components/{id}/vulnerabilities`, `/vulnerabilities`, `/cve-allowlists`, `/scan-policies`, `/trust-policies`, `/trust-materials`, `/components/{id}/sbom`, `/components/{id}/accessories`, `/vulnerabilities/exports` |
| `rustryx-config` | the `scan.*` keys below |

New configuration keys: `scan.enabled` (bool, default false: no scanner starts and every component reads as not scanned), `scan.trivy_path`, `scan.osv_scanner_path`, `scan.cosign_path`, `scan.notation_path` (absolute paths, verified readable and executable at startup, delivered by an init container with a pinned digest as DSO-22 requires), `scan.work_dir` (default `/var/lib/rustryx/scan`, an `emptyDir`), `scan.work_dir_max_bytes` (default 20 GiB), `scan.max_concurrent` (2), `scan.timeout_secs` (600), `scan.max_report_bytes` (256 MiB), `scan.enqueue_per_second` (200), `scan.rescan_after_days` (7), `scan.dataset_sync_secs` (300), `scan.osv_mode` (`offline` or `online`, default `offline`), `scan.osv_mirror_url`, `scan.trivy_db_repository`, `scan.max_findings_per_report` (50 000).

Memory and disk sizing note for the Helm chart: the scan work directory holds one exported image per running scan, so `scan.max_concurrent x largest image` must fit `scan.work_dir_max_bytes`; the Trivy process needs memory in proportion to the image (a spike measurement, section 10), so the pod memory limit grows with `scan.max_concurrent`.

REST resources: `GET/POST /api/v1/scanners`, `PUT/DELETE /api/v1/scanners/{id}`, `POST /api/v1/scanners/{id}/actions/probe`, `POST /api/v1/scans` (one or several components), `POST /api/v1/scans/actions/scan-all`, `POST /api/v1/scans/{id}/actions/stop`, `GET /api/v1/components/{id}/vulnerabilities[/summary]`, `GET /api/v1/vulnerabilities?cve=`, `POST /api/v1/vulnerabilities/exports`, `GET /api/v1/vulnerabilities/exports/{id}/download` (once), `GET/POST/DELETE /api/v1/cve-allowlists`, `GET/PUT /api/v1/scan-policies/{scope}`, `GET/PUT /api/v1/trust-policies/{scope}`, `GET/POST/DELETE /api/v1/trust-materials`, `GET /api/v1/repositories/{name}/health`, `POST /api/v1/components/{id}/actions/release-quarantine`.

The HTTP scanner contract that Rustryx defines for the `http` adapter (the reference product's own adapter API is `unverified`, so no interoperability is promised):

```text
GET  {endpoint}/v1/health                 -> 200 {"ok": true, "version": "...", "dataset": "..."}
POST {endpoint}/v1/scans                  -> 202 {"id": "<scanner side id>"}
       {"kind": "vulnerability" | "sbom",
        "subject": {"type": "packages", "purls": ["pkg:maven/..."]}
                 | {"type": "oci", "registry": "https://<node>", "repository": "<name>", "digest": "sha256:...",
                    "access_token": "<scan token, 15 minutes, read of that repository>"}}
GET  {endpoint}/v1/scans/{id}             -> 200 {"status": "pending|running|complete|failed", "error": "...",
                                                  "findings": [{"id","package","version","fixed","severity","cvss","title"}],
                                                  "sbom": {"format": "cyclonedx-json|spdx-json", "document": {...}}}
```

Polling is every 2 s up to `scan.timeout_secs`; the response size is capped; every field is length bounded and sanitized before it is stored; auth is the configured `ScannerAuth`; the call goes through `Purpose::Scanner` egress.

### 5.h Negative requirements

| Requirement | Test |
|---|---|
| The pull gate fails closed: a missing, failed or unreadable verdict denies in `Enforce` mode | `scan::must_not_allow_a_pull_when_the_verdict_is_unknown_in_enforce`, `..._when_the_store_is_down` (503, never allow) |
| An unscanned component is not clean | `scan::must_not_report_an_unscanned_component_as_clean`, `..._when_the_scanner_is_down` |
| `AuditOnly` never blocks except a quarantine, and always writes the audit row | `scan::must_not_block_in_audit_only`, `..._skip_the_audit_row` |
| `Negligible` never blocks and `Unknown` is not a bypass | `scan::must_not_block_on_negligible`, `..._let_an_unknown_severity_bypass_the_gate` |
| An index is judged by its children; a child pull by its own verdict | `scan::must_not_allow_an_index_with_a_blocked_child` |
| An expired allowlist entry stops counting at once; an allowlist of another scope never applies | `scan::must_not_honour_an_expired_exception`, `..._apply_another_projects_allowlist` |
| A policy or allowlist change reaches the gate within the bound (bus and TTL) | `scan::must_not_serve_a_stale_allow_beyond_the_ttl` |
| The scanner token cannot read another repository, outlives 15 minutes, or reach any non OCI read route | `rbac::must_not_let_a_scan_token_read_another_repository`, `auth::must_not_accept_an_expired_scan_token` |
| Scanner reads are not counted and do not refresh pull time | `format_core::must_not_count_scan_token_reads` |
| No scanner argument comes from request data; no shell; the environment is cleared; the work directory is removed on success, error, timeout and cancel | `scan::must_not_pass_untrusted_strings_to_a_process`, `..._leave_a_work_directory_behind`, `..._inherit_the_environment` |
| A scan that exceeds its timeout or output cap is killed and recorded as failed | `scan::must_kill_a_scan_after_timeout`, `..._cap_scanner_output` |
| A scanner credential never appears in responses, logs or errors | `api::must_not_return_a_scanner_secret`, `scan::must_not_log_a_scanner_credential` |
| A scanner endpoint is SSRF checked on create and on every call; no skip-verify flag exists | `scan::must_not_call_an_internal_scanner_endpoint`, `api::must_not_accept_a_skip_verify_flag` |
| Quarantine hides a new proxy version until a scan passes, and the fill request itself gets 403 | `format_core::must_not_serve_a_quarantined_component`, `..._race_between_fill_and_scan` |
| A trust policy never accepts a signature by presence alone | `scan::must_not_accept_an_unverified_signature`, `..._a_signature_by_an_untrusted_key`, `..._a_signature_over_another_digest`, `..._an_expired_certificate` |
| `Both` needs both schemes to verify | `scan::must_not_accept_both_when_one_scheme_fails` |
| A private key in uploaded trust material is refused | `api::must_not_accept_private_key_material` |
| Security Hub search without a CVE id is refused | `db::must_not_scan_all_findings_without_a_cve_id` |
| A findings replace is atomic: a reader never sees the old report's findings with the new summary | `db::must_not_expose_a_half_replaced_report` |
| An export can be downloaded once, by its requester, and expires | `api::must_not_download_an_export_twice`, `..._someone_elses_export` |
| Enforce mode and quarantine cannot be enabled while an older node is live | `api::must_not_enable_enforce_while_an_older_node_is_live` |

---

## 6. W2-E: distribution

Capabilities: `PRX-14` to `PRX-21`, `PRX-23`, `OPS-01` to `OPS-07`, `UPL-12`, `UPL-13`, DSO-28, DSO-32. Decisions of parity sections 11.6, 11.8 and 11.11 are made here. New crates `rustryx-notify` (webhooks, e-mail, outbox consumers) and `rustryx-replication` (registry client, replication, promotion executor).

Sub-steps: **E1** registries and webhooks with the outbox consumer and the signer (needs C0); **E2** replication (needs E1, the OCI crate, labels from B), e-mail (needs the SMTP settings of H); **E3** promotion and copy (needs the gate and checks of D, immutability and holds of C).

Everything in this stream calls out to addresses that an administrator typed, so every call goes through `rustryx-egress` with a per purpose policy (section 2.5): a registry or a webhook is never allowed what an internal mirror is.

### 6.a Entities and enums

```rust
// core: crates/rustryx-core/src/registry.rs  (new file)
/// `Rustryx` additionally negotiates the replication protocol version with the peer (`PRX-21`); `Oci` is any
/// distribution compatible registry. Vendor token exchange adapters are `later`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryProvider { Rustryx, Oci }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RegistryAuth {
    None,
    /// Also used for the Docker token handshake: the client follows the `WWW-Authenticate: Bearer` challenge itself.
    Basic { username: String, password: Secret },
    /// A static bearer token (for example a Rustryx robot token used as a password is `Basic`).
    Bearer { token: Secret },
}

/// A named, reusable remote registry. Proxy repositories, replication rules and scanners reference it by id. There is no
/// "verify certificate" switch: a private CA goes through `ca_certificate_ids` (managed trust store, section 8).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Registry {
    pub id: RegistryId,
    pub name: String,
    pub provider: RegistryProvider,
    /// `https` origin only: no path, query, fragment or user info. `http` only for hosts of `egress.registries.allow_insecure_http_hosts`.
    pub url: String,
    pub auth: RegistryAuth,
    pub ca_certificate_ids: Vec<TrustedCertificateId>,
    pub last_test: Option<RegistryTest>,
    pub version: i64,
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryTest { pub at: OffsetDateTime, pub ok: bool, pub protocol_version: Option<u32>, pub message: String }

// ProxyConfig (model.rs): two more additive fields, used by OCI proxies (parity 11.11).
//     /// Globs on the image path; empty allows everything and the UI warns, because the upstream account defines exposure.
//     pub namespace_allowlist: Vec<String>,
//     /// Pull speed limit towards the upstream, kilobytes per second; `None` unlimited.
//     pub bandwidth_limit_kbps: Option<u32>,
```

```rust
// core: crates/rustryx-core/src/replication.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationDirection { Push, Pull }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceFilter { Image, Chart, Any }

/// Filters. Globs are the selector syntax of section 4.a. A label filter keeps only components that carry one of the labels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplicationFilter {
    pub name_globs: Vec<String>,
    pub tag_globs: Vec<String>,
    pub label_ids: Vec<LabelId>,
    pub resource: Option<ResourceFilter>,
}

/// `Scheduled` rules keep their cron in a `task_definitions` row of kind `replication` (`schedule_task_id`), so schedules
/// of every kind live in one place (section 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ReplicationTrigger { Manual, Scheduled, Event { on_push: bool, on_delete: bool } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReplicationScope { Repository(RepositoryId), Project(ProjectId) }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReplicationRule {
    pub id: ReplicationRuleId,
    pub name: String,
    pub direction: ReplicationDirection,
    pub registry_id: RegistryId,
    pub local: ReplicationScope,
    /// Prefix on the remote side (push: where it goes; pull: where it comes from).
    pub remote_namespace: String,
    /// Leading path segments of the image path dropped on the destination, 0 to 3.
    pub flatten_levels: u8,
    pub override_existing: bool,
    pub filter: ReplicationFilter,
    pub trigger: ReplicationTrigger,
    pub schedule_task_id: Option<TaskDefinitionId>,
    /// Copy signatures and SBOMs (referrers) after the subject. Default true.
    pub copy_referrers: bool,
    /// Event rules only: propagate deletes. Needs `nx-admin` and is audited.
    pub replicate_deletion: bool,
    pub bandwidth_limit_kbps: Option<u32>,
    /// Event rules: wait this long after a push so a signature pushed right after can travel with it. Default 30.
    pub event_settle_secs: u32,
    pub enabled: bool,
    pub version: i64,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStatus { Queued, Running, Succeeded, PartiallyFailed, Failed, Stopped }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionCounts { pub total: u32, pub succeeded: u32, pub failed: u32, pub skipped: u32, pub in_progress: u32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReplicationExecution {
    pub id: ReplicationExecutionId,
    pub rule_id: ReplicationRuleId,
    pub trigger: TaskTrigger,
    pub status: ExecutionStatus,
    pub counts: ExecutionCounts,
    pub stop_requested: bool,
    pub requested_by: Option<UserId>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationTaskKind { Copy, CopyReferrers, Delete }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationTaskStatus { Pending, Running, Succeeded, Failed, Skipped, Stopped }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReplicationTask {
    pub id: ReplicationTaskId,
    pub execution_id: ReplicationExecutionId,
    pub kind: ReplicationTaskKind,
    pub repository: String,
    pub image: String,
    /// Tag or digest.
    pub reference: String,
    pub status: ReplicationTaskStatus,
    pub attempts: u8,
    pub bytes_transferred: u64,
    /// Short stable code (`immutable`, `quota`, `blocked_by_gate`, `remote_error`, `timeout`, `digest_mismatch`, ...), never remote text.
    pub error: Option<String>,
    pub available_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}
```

```rust
// core: crates/rustryx-core/src/webhook.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookKind { Http, CloudEvents }          // Slack formatting is `later`

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum NotifyScope { System, Project(ProjectId), Repository(RepositoryId) }

/// The secret is mandatory (DSO-32): a webhook without one cannot be created. Both secret fields are sealed at rest and are
/// shown once, at creation or rotation, as `SecretStatus` afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Webhook {
    pub id: WebhookId,
    pub scope: NotifyScope,
    pub name: String,
    pub kind: WebhookKind,
    /// `https`, no user info, no fragment; checked against the `Webhook` egress policy on create, update and on every delivery.
    pub url: String,
    pub secret: Secret,
    /// Optional value of an `Authorization` header sent with every delivery.
    pub auth_header: Option<Secret>,
    /// 1 to 32 subscribable kinds. A webhook cannot subscribe to the internal kinds.
    pub events: Vec<EventKind>,
    pub enabled: bool,
    /// Set after `webhooks.suspend_after_failures` (default 100) consecutive dead deliveries. A suspended webhook gets no
    /// new deliveries until re-enabled.
    pub suspended_at: Option<OffsetDateTime>,
    pub consecutive_failures: u32,
    /// Total attempts per delivery, 1 to 10, default 5.
    pub retry_max: u8,
    /// Per attempt total timeout, 1 to 30 seconds, default 10.
    pub timeout_secs: u8,
    pub version: i64,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus { Pending, Delivering, Succeeded, Failed, Dead }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WebhookDelivery {
    pub id: WebhookDeliveryId,
    pub webhook_id: WebhookId,
    pub event_id: EventId,
    pub event_kind: EventKind,
    pub attempt: u8,
    pub status: DeliveryStatus,
    pub next_attempt_at: Option<OffsetDateTime>,
    pub last_status_code: Option<u16>,
    pub last_latency_ms: Option<u32>,
    /// Stable code (`timeout`, `connect`, `tls`, `blocked_by_policy`, `http_status`, `redirect_refused`), never a remote message.
    pub last_error: Option<String>,
    /// First 1 KiB of the response body, control characters removed. Never includes request headers.
    pub response_excerpt: Option<String>,
    pub created_at: OffsetDateTime,
    pub delivered_at: Option<OffsetDateTime>,
}
```

```rust
// core: crates/rustryx-core/src/notification.rs  (new file)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Recipient {
    /// Syntax checked, at most 254 characters. Not used for any access decision (users.email never is).
    Address { address: String },
    /// Members (direct rows) of the subscription's project with that role or above.
    ProjectRole { role: ProjectRole },
    /// For `token.expiring`: the owner or creator of the token.
    TokenOwner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NotificationSubscription {
    pub id: SubscriptionId,
    pub scope: NotifyScope,
    /// Includes the internal kinds `task.finished` and `token.expiring`.
    pub events: Vec<EventKind>,
    /// At most 20.
    pub recipients: Vec<Recipient>,
    pub enabled: bool,
    pub version: i64,
    pub created_by: Option<UserId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailStatus { Pending, Sending, Sent, Failed, Dead, Suppressed }

pub struct EmailDelivery { pub id: EmailDeliveryId, pub subscription_id: Option<SubscriptionId>, pub event_id: EventId,
                           pub to: String, pub status: EmailStatus, pub attempts: u8, pub next_attempt_at: Option<OffsetDateTime>,
                           pub last_error: Option<String>, pub created_at: OffsetDateTime, pub sent_at: Option<OffsetDateTime> }
```

```rust
// core: crates/rustryx-core/src/promotion.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionMode { Copy, Move }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum PromotionSelector {
    /// At most 10 000 per request (`UPL-12`).
    Components { ids: Vec<ComponentId> },
    Label { label_id: LabelId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum PromotionCheck { ScanBelowThreshold { threshold: Severity }, SignatureValid, SbomPresent }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PromotionStatus { PendingApproval, Approved, Rejected, Running, Succeeded, PartiallyFailed, Failed, Cancelled, Expired }

/// A release repository's promotion rule (DSO-28). A target without a row accepts direct copies from users with
/// read on the source and write on the target; a row with `require_approval` refuses them (`ApprovalRequired`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromotionTarget {
    pub repository_id: RepositoryId,
    pub require_approval: bool,
    /// At least 1 when `require_approval`.
    pub required_approvals: u8,
    pub checks: Vec<PromotionCheck>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromotionRequest {
    pub id: PromotionId,
    pub mode: PromotionMode,
    pub source_repository_id: RepositoryId,
    pub target_repository_id: RepositoryId,
    pub selector: PromotionSelector,
    pub requested_by: UserId,
    pub requested_at: OffsetDateTime,
    pub status: PromotionStatus,
    pub required_approvals: u8,
    pub checks: Vec<PromotionCheck>,
    /// A pending request expires after 14 days.
    pub expires_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

/// A decision counts only if the approver is not the requester and not the author of any selected component (the author is
/// `components.created_by`). A decision that does not count is stored with `counted = false` and shown, so nothing is hidden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionDecision { pub promotion_id: PromotionId, pub approver: UserId, pub approve: bool,
                               pub comment: String, pub counted: bool, pub at: OffsetDateTime }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionItemStatus { Pending, Done, Skipped, Failed }
pub struct PromotionItem { pub promotion_id: PromotionId, pub component_id: ComponentId, pub status: PromotionItemStatus,
                           /// `immutable`, `held`, `quota`, `check_failed`, `target_exists`, ... never free text.
                           pub reason: Option<String> }
```

### 6.b Ports

```rust
// core: crates/rustryx-core/src/ports/distribution.rs  (new file)
// Contract as the other metadata ports. Methods that take `advance` move an outbox cursor in the same transaction.

#[async_trait]
pub trait RegistryRepo: Send + Sync {
    async fn create(&self, registry: &Registry) -> Result<(), StoreError>;           // credentials sealed (purpose registry.credential)
    async fn get(&self, id: RegistryId) -> Result<Option<Registry>, StoreError>;     // credentials unsealed for runtime use
    async fn list(&self) -> Result<Vec<Registry>, StoreError>;
    async fn update(&self, registry: &Registry, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a proxy repository, a replication rule or a scanner uses it; `dependents` names them.
    async fn delete(&self, id: RegistryId) -> Result<(), StoreError>;
    async fn dependents(&self, id: RegistryId) -> Result<RegistryDependents, StoreError>;
    async fn record_test(&self, id: RegistryId, test: &RegistryTest) -> Result<(), StoreError>;
}
pub struct RegistryDependents { pub proxy_repositories: Vec<RepositoryId>, pub replication_rules: Vec<ReplicationRuleId> }

#[async_trait]
pub trait ReplicationRepo: Send + Sync {
    async fn create_rule(&self, rule: &ReplicationRule) -> Result<(), StoreError>;
    async fn get_rule(&self, id: ReplicationRuleId) -> Result<Option<ReplicationRule>, StoreError>;
    async fn list_rules(&self) -> Result<Vec<ReplicationRule>, StoreError>;
    /// `Conflict` while an execution of the rule is queued or running (the rule cannot change under a run).
    async fn update_rule(&self, rule: &ReplicationRule, expected_version: i64) -> Result<(), StoreError>;
    async fn delete_rule(&self, id: ReplicationRuleId) -> Result<(), StoreError>;
    /// `None` when an execution of that rule is queued or running (the partial unique index): one active run per rule.
    async fn start_execution(&self, execution: &ReplicationExecution) -> Result<Option<ReplicationExecution>, StoreError>;
    async fn get_execution(&self, id: ReplicationExecutionId) -> Result<Option<ReplicationExecution>, StoreError>;
    async fn list_executions(&self, rule: Option<ReplicationRuleId>, page: &PageRequest) -> Result<Page<ReplicationExecution>, StoreError>;
    async fn request_stop(&self, id: ReplicationExecutionId) -> Result<bool, StoreError>;
    /// Adds tasks to an execution (at most 1000 per call).
    async fn add_tasks(&self, tasks: &[ReplicationTask]) -> Result<(), StoreError>;
    /// Event triggered rules: creates tasks for matching events in the rule's open event execution (created if none was
    /// open in the last minute) and advances the cursor, in one transaction.
    async fn create_event_tasks(&self, rule: ReplicationRuleId, tasks: &[ReplicationTask], advance: Option<&CursorAdvance>) -> Result<(), StoreError>;
    /// `SKIP LOCKED` claim of a due task of a running execution; lease 10 minutes.
    async fn claim_task(&self, owner: &str, lease: Duration) -> Result<Option<ReplicationTask>, StoreError>;
    async fn heartbeat_task(&self, id: ReplicationTaskId, owner: &str, lease: Duration) -> Result<bool, StoreError>;
    async fn finish_task(&self, id: ReplicationTaskId, owner: &str, status: ReplicationTaskStatus, bytes: u64,
                         error: Option<&str>, retry_in: Option<Duration>, events: &[EventDraft]) -> Result<bool, StoreError>;
    async fn tasks(&self, execution: ReplicationExecutionId, page: &PageRequest) -> Result<Page<ReplicationTask>, StoreError>;
    async fn append_task_log(&self, id: ReplicationTaskId, lines: &[TaskLogLine]) -> Result<(), StoreError>;
    async fn read_task_log(&self, id: ReplicationTaskId, after_seq: i32, limit: u32) -> Result<Vec<TaskLogLine>, StoreError>;
    /// Marks executions with no unfinished task as finished and writes the `replication.finished` event in the same transaction.
    async fn settle_executions(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

#[async_trait]
pub trait WebhookRepo: Send + Sync {
    async fn create(&self, webhook: &Webhook) -> Result<(), StoreError>;             // secret and auth header sealed
    async fn get(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError>;
    async fn list(&self, scope: Option<NotifyScope>) -> Result<Vec<Webhook>, StoreError>;   // at most 20 per scope: Conflict beyond
    async fn update(&self, webhook: &Webhook, expected_version: i64) -> Result<(), StoreError>;
    async fn delete(&self, id: WebhookId) -> Result<(), StoreError>;
    /// New secret, old one invalid at once. Returns the version.
    async fn rotate_secret(&self, id: WebhookId, secret: &Secret, expected_version: i64) -> Result<i64, StoreError>;
    /// Enabled, not suspended webhooks that would receive an event (system, or its project, or its repository).
    async fn matching(&self, kind: EventKind, project: Option<ProjectId>, repository: Option<RepositoryId>) -> Result<Vec<Webhook>, StoreError>;
    /// True when any enabled webhook subscribes to `Pull`; cached 60 s by the caller.
    async fn any_subscribes_to(&self, kind: EventKind) -> Result<bool, StoreError>;
    async fn record_outcome(&self, id: WebhookId, success: bool, suspend_after: u32) -> Result<(), StoreError>;
}

#[async_trait]
pub trait WebhookDeliveryRepo: Send + Sync {
    /// One pending delivery per `(webhook, event)` and the cursor advance, in one transaction (the outbox hand-off).
    async fn create_for_events(&self, rows: &[(WebhookId, EventId, EventKind)], advance: Option<&CursorAdvance>) -> Result<u64, StoreError>;
    /// `SKIP LOCKED` claim of a due `pending` row, or a `delivering` row whose lease expired; lease 60 s.
    async fn claim(&self, owner: &str, lease: Duration) -> Result<Option<WebhookDelivery>, StoreError>;
    async fn finish(&self, id: WebhookDeliveryId, owner: &str, outcome: &DeliveryOutcome) -> Result<bool, StoreError>;
    async fn list(&self, webhook: WebhookId, page: &PageRequest) -> Result<Page<WebhookDelivery>, StoreError>;
    /// Counts for `webhook_deliveries_pending`.
    async fn pending_count(&self) -> Result<u64, StoreError>;
    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError>;
}
pub struct DeliveryOutcome { pub status: DeliveryStatus, pub status_code: Option<u16>, pub latency_ms: Option<u32>,
                             pub error: Option<String>, pub excerpt: Option<String>, pub next_attempt_at: Option<OffsetDateTime> }

#[async_trait]
pub trait NotificationRepo: Send + Sync {
    async fn upsert(&self, subscription: &NotificationSubscription, expected_version: Option<i64>) -> Result<(), StoreError>;
    async fn list(&self, scope: Option<NotifyScope>) -> Result<Vec<NotificationSubscription>, StoreError>;
    async fn delete(&self, id: SubscriptionId) -> Result<(), StoreError>;
    async fn matching(&self, kind: EventKind, project: Option<ProjectId>, repository: Option<RepositoryId>) -> Result<Vec<NotificationSubscription>, StoreError>;
    /// Resolved recipients as addresses, with the cursor advance in one transaction; identical `(address, event kind, subject key)`
    /// within 10 minutes are `Suppressed` rows.
    async fn create_for_events(&self, rows: &[NewEmail], advance: Option<&CursorAdvance>) -> Result<u64, StoreError>;
    async fn claim(&self, owner: &str, lease: Duration) -> Result<Option<EmailDelivery>, StoreError>;
    async fn finish(&self, id: EmailDeliveryId, owner: &str, status: EmailStatus, error: Option<&str>, retry_in: Option<Duration>) -> Result<bool, StoreError>;
    async fn count_sent_since(&self, since: OffsetDateTime) -> Result<u64, StoreError>;
    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError>;
}
pub struct NewEmail { pub subscription_id: Option<SubscriptionId>, pub event_id: EventId, pub to: String, pub kind: EventKind, pub subject_key: String }

#[async_trait]
pub trait PromotionRepo: Send + Sync {
    async fn set_target(&self, target: &PromotionTarget, expected_version: Option<i64>) -> Result<(), StoreError>;
    async fn get_target(&self, repository: RepositoryId) -> Result<Option<PromotionTarget>, StoreError>;
    /// Creates the request and its items (at most 10 000) in one transaction, writes the `promotion.requested` event.
    async fn create(&self, request: &PromotionRequest, items: &[ComponentId], events: &[EventDraft]) -> Result<(), StoreError>;
    async fn get(&self, id: PromotionId) -> Result<Option<PromotionRequest>, StoreError>;
    async fn list(&self, repository: Option<RepositoryId>, status: Option<PromotionStatus>, page: &PageRequest) -> Result<Page<PromotionRequest>, StoreError>;
    /// Stores the decision (marking whether it counts) and, when the counted approvals reach the requirement, moves the
    /// request to `Approved` and enqueues the executor job `promotion:<id>`, all in one transaction. A rejection ends it.
    async fn decide(&self, decision: &PromotionDecision, events: &[EventDraft]) -> Result<PromotionStatus, StoreError>;
    async fn decisions(&self, id: PromotionId) -> Result<Vec<PromotionDecision>, StoreError>;
    async fn cancel(&self, id: PromotionId, by: UserId) -> Result<(), StoreError>;       // only the requester, only while pending
    async fn items(&self, id: PromotionId, page: &PageRequest) -> Result<Page<PromotionItem>, StoreError>;
    async fn mark_running(&self, id: PromotionId, at: OffsetDateTime) -> Result<bool, StoreError>;
    async fn set_item(&self, id: PromotionId, component: ComponentId, status: PromotionItemStatus, reason: Option<&str>) -> Result<(), StoreError>;
    async fn finish(&self, id: PromotionId, status: PromotionStatus, at: OffsetDateTime, events: &[EventDraft]) -> Result<(), StoreError>;
    async fn expire(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
}
```

Crate level contracts (not core):

```rust
// crate: rustryx-notify
/// `sha256=<hex>` of HMAC-SHA256 over `"<timestamp>." + body`. The timestamp is in the signed bytes, so a replayed body
/// with a new timestamp does not verify (SEC-38).
pub fn sign(secret: &[u8], timestamp: i64, body: &[u8]) -> String;
/// The reference verifier for receivers, also used by the self check of the test endpoint. Constant time comparison
/// (`hmac::Mac::verify_slice` or `subtle`), rejects a timestamp older or newer than `tolerance_secs` (default 300).
pub fn verify(secret: &[u8], timestamp_header: &str, signature_header: &str, body: &[u8], now: i64, tolerance_secs: i64)
    -> Result<(), SignatureError>;

// crate: rustryx-replication
/// The remote side of a copy. One implementation speaks OCI distribution over `Purpose::Registry` egress.
#[async_trait]
pub trait RegistryClient: Send + Sync {
    async fn ping(&self) -> Result<RegistryPing, UpstreamError>;
    async fn list_repositories(&self, prefix: &str, page: Option<&str>) -> Result<NamePage, UpstreamError>;
    async fn list_tags(&self, repository: &str, page: Option<&str>) -> Result<NamePage, UpstreamError>;
    async fn manifest(&self, repository: &str, reference: &str) -> Result<RemoteManifest, UpstreamError>;     // size capped at 4 MiB
    async fn blob_exists(&self, repository: &str, digest: &str) -> Result<bool, UpstreamError>;
    async fn pull_blob(&self, repository: &str, digest: &str, limiter: &BandwidthLimiter)
        -> Result<BoxStream<'static, Result<Bytes, UpstreamError>>, UpstreamError>;
    /// Monolithic or chunked upload. Not retried inside the client (retries are only for GET and HEAD); the task retries the blob.
    async fn push_blob(&self, repository: &str, digest: &str, size: u64, body: BoxStream<'static, Result<Bytes, UpstreamError>>,
                       limiter: &BandwidthLimiter) -> Result<(), UpstreamError>;
    async fn put_manifest(&self, repository: &str, reference: &str, media_type: &str, body: Bytes) -> Result<(), UpstreamError>;
    async fn referrers(&self, repository: &str, digest: &str) -> Result<Vec<RemoteDescriptor>, UpstreamError>;
    async fn delete_manifest(&self, repository: &str, digest: &str) -> Result<(), UpstreamError>;
}
```

### 6.c PostgreSQL

Migrations `0080_registries.sql`, `0081_replication.sql`, `0082_webhooks.sql`, `0083_notifications.sql`, `0084_promotion.sql`. The outbox tables are in the foundation migration.

```sql
-- 0080
CREATE TABLE registries (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    provider text NOT NULL CHECK (provider IN ('rustryx', 'oci')),
    url text NOT NULL CHECK (url ~ '^https?://[^/@?#]+$'),            -- origin only; http needs the egress allowlist, checked in Rust
    auth jsonb NOT NULL DEFAULT '{"type":"none"}'::jsonb,           -- secrets inside are sealed envelopes (registry.credential)
    ca_certificate_ids uuid[] NOT NULL DEFAULT '{}',
    last_test jsonb,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_registries_name UNIQUE (name)
);
ALTER TABLE repositories ADD COLUMN registry_id uuid REFERENCES registries (id) ON DELETE RESTRICT;   -- proxy repositories; NULL keeps the inline remote_url
CREATE INDEX ix_repositories_registry ON repositories (registry_id) WHERE registry_id IS NOT NULL;

-- 0081
CREATE TABLE replication_rules (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    direction text NOT NULL CHECK (direction IN ('push', 'pull')),
    registry_id uuid NOT NULL REFERENCES registries (id) ON DELETE RESTRICT,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    remote_namespace text NOT NULL DEFAULT '' CHECK (length(remote_namespace) <= 255),
    flatten_levels smallint NOT NULL DEFAULT 0 CHECK (flatten_levels BETWEEN 0 AND 3),
    override_existing boolean NOT NULL DEFAULT false,
    filter jsonb NOT NULL DEFAULT '{}'::jsonb,
    trigger text NOT NULL CHECK (trigger IN ('manual', 'scheduled', 'event')),
    on_push boolean NOT NULL DEFAULT false, on_delete boolean NOT NULL DEFAULT false,
    schedule_task_id uuid,
    copy_referrers boolean NOT NULL DEFAULT true,
    replicate_deletion boolean NOT NULL DEFAULT false,
    bandwidth_limit_kbps integer CHECK (bandwidth_limit_kbps IS NULL OR bandwidth_limit_kbps > 0),
    event_settle_secs integer NOT NULL DEFAULT 30 CHECK (event_settle_secs BETWEEN 0 AND 600),
    enabled boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1,
    created_by uuid, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_replication_rules_name UNIQUE (name),
    CONSTRAINT ck_replication_scope CHECK ((repository_id IS NULL) <> (project_id IS NULL))
);
CREATE TABLE replication_executions (
    id uuid PRIMARY KEY,
    rule_id uuid NOT NULL REFERENCES replication_rules (id) ON DELETE CASCADE,
    trigger text NOT NULL,
    status text NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'partially_failed', 'failed', 'stopped')),
    counts jsonb NOT NULL DEFAULT '{}'::jsonb,
    stop_requested_at timestamptz,
    requested_by uuid,
    started_at timestamptz, finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- One active execution per rule: the database side of "one active run per rule" (PRX-16), no lock connection needed.
CREATE UNIQUE INDEX uq_replication_active ON replication_executions (rule_id) WHERE status IN ('queued', 'running');
CREATE INDEX ix_replication_exec_rule ON replication_executions (rule_id, created_at DESC);
CREATE TABLE replication_tasks (
    id uuid PRIMARY KEY,
    execution_id uuid NOT NULL REFERENCES replication_executions (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('copy', 'copy_referrers', 'delete')),
    repository text NOT NULL, image text NOT NULL, reference text NOT NULL,
    status text NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'skipped', 'stopped')),
    attempts smallint NOT NULL DEFAULT 0,
    bytes_transferred bigint NOT NULL DEFAULT 0,
    error text CHECK (error IS NULL OR length(error) <= 64),
    available_at timestamptz NOT NULL DEFAULT now(),
    lease_owner text, lease_expires_at timestamptz,
    started_at timestamptz, finished_at timestamptz
);
CREATE INDEX ix_replication_tasks_claim ON replication_tasks (available_at) WHERE status = 'pending';
CREATE INDEX ix_replication_tasks_exec ON replication_tasks (execution_id, status);
CREATE TABLE replication_task_log (task_id uuid NOT NULL REFERENCES replication_tasks (id) ON DELETE CASCADE, seq integer NOT NULL,
    at timestamptz NOT NULL DEFAULT now(), level smallint NOT NULL, message text NOT NULL CHECK (length(message) <= 1024),
    PRIMARY KEY (task_id, seq));
-- Executions, tasks and logs are kept 90 days after finish (task_purge).

-- 0082
CREATE TABLE webhooks (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    kind text NOT NULL CHECK (kind IN ('http', 'cloudevents')),
    url text NOT NULL CHECK (url ~ '^https?://[^@]+$' AND length(url) <= 2048),
    secret text NOT NULL,                       -- sealed envelope, NOT NULL: no webhook without a secret (DSO-32)
    auth_header text,                           -- sealed envelope
    events text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 32),
    enabled boolean NOT NULL DEFAULT true,
    suspended_at timestamptz,
    consecutive_failures integer NOT NULL DEFAULT 0,
    retry_max smallint NOT NULL DEFAULT 5 CHECK (retry_max BETWEEN 1 AND 10),
    timeout_secs smallint NOT NULL DEFAULT 10 CHECK (timeout_secs BETWEEN 1 AND 30),
    version bigint NOT NULL DEFAULT 1,
    created_by uuid, created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_webhooks_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);
CREATE INDEX ix_webhooks_project ON webhooks (project_id) WHERE project_id IS NOT NULL AND enabled;
CREATE INDEX ix_webhooks_repository ON webhooks (repository_id) WHERE repository_id IS NOT NULL AND enabled;
CREATE INDEX ix_webhooks_system ON webhooks (system) WHERE system AND enabled;
-- At most 20 webhooks per scope (BEFORE INSERT trigger under the scope's advisory transaction lock).

CREATE TABLE webhook_deliveries (
    id uuid NOT NULL,
    webhook_id uuid NOT NULL,                  -- no foreign key: deleting a webhook must not scan the partitions
    event_id uuid NOT NULL,
    event_kind text NOT NULL,
    attempt smallint NOT NULL DEFAULT 0,
    status text NOT NULL CHECK (status IN ('pending', 'delivering', 'succeeded', 'failed', 'dead')),
    next_attempt_at timestamptz,
    lease_owner text, lease_expires_at timestamptz,
    last_status_code smallint, last_latency_ms integer,
    last_error text CHECK (last_error IS NULL OR length(last_error) <= 64),
    response_excerpt text CHECK (response_excerpt IS NULL OR length(response_excerpt) <= 1024),
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    PRIMARY KEY (created_at, id)
) PARTITION BY RANGE (created_at);
CREATE UNIQUE INDEX uq_webhook_delivery_event ON webhook_deliveries (webhook_id, event_id, created_at);   -- one delivery per webhook and event
CREATE INDEX ix_webhook_deliveries_claim ON webhook_deliveries (next_attempt_at) WHERE status IN ('pending', 'delivering');
CREATE INDEX ix_webhook_deliveries_webhook ON webhook_deliveries (webhook_id, created_at DESC);
-- Monthly partitions, ensure_webhook_partitions and drop_old_webhook_partitions(retention_days = 30) like the audit functions.

-- 0083
CREATE TABLE notification_subscriptions (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    events text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 32),
    recipients jsonb NOT NULL CHECK (jsonb_typeof(recipients) = 'array' AND jsonb_array_length(recipients) BETWEEN 1 AND 20),
    enabled boolean NOT NULL DEFAULT true, version bigint NOT NULL DEFAULT 1, created_by uuid,
    CONSTRAINT ck_notify_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);
CREATE TABLE email_deliveries (
    id uuid PRIMARY KEY,
    subscription_id uuid REFERENCES notification_subscriptions (id) ON DELETE SET NULL,
    event_id uuid NOT NULL, event_kind text NOT NULL, subject_key text NOT NULL,
    to_address text NOT NULL CHECK (length(to_address) <= 254),
    status text NOT NULL, attempts smallint NOT NULL DEFAULT 0,
    next_attempt_at timestamptz, lease_owner text, lease_expires_at timestamptz,
    last_error text CHECK (last_error IS NULL OR length(last_error) <= 64),
    created_at timestamptz NOT NULL DEFAULT now(), sent_at timestamptz
);
CREATE INDEX ix_email_claim ON email_deliveries (next_attempt_at) WHERE status IN ('pending', 'sending');
CREATE INDEX ix_email_dedupe ON email_deliveries (to_address, event_kind, subject_key, created_at DESC);
-- Kept 30 days after creation.

-- 0084
CREATE TABLE promotion_targets (
    repository_id uuid PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    require_approval boolean NOT NULL, required_approvals smallint NOT NULL DEFAULT 1,
    checks jsonb NOT NULL DEFAULT '[]'::jsonb, version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_promotion_target CHECK (NOT require_approval OR required_approvals >= 1)
);
CREATE TABLE promotion_requests (
    id uuid PRIMARY KEY,
    mode text NOT NULL CHECK (mode IN ('copy', 'move')),
    source_repository_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    target_repository_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    selector jsonb NOT NULL,
    requested_by uuid NOT NULL, requested_at timestamptz NOT NULL DEFAULT now(),
    status text NOT NULL, required_approvals smallint NOT NULL, checks jsonb NOT NULL DEFAULT '[]'::jsonb,
    expires_at timestamptz NOT NULL, started_at timestamptz, finished_at timestamptz,
    CONSTRAINT ck_promotion_distinct CHECK (source_repository_id <> target_repository_id)
);
CREATE INDEX ix_promotion_status ON promotion_requests (status, requested_at DESC);
CREATE TABLE promotion_items (
    promotion_id uuid NOT NULL REFERENCES promotion_requests (id) ON DELETE CASCADE,
    component_id uuid NOT NULL, status text NOT NULL, reason text CHECK (reason IS NULL OR length(reason) <= 64),
    PRIMARY KEY (promotion_id, component_id)
);
CREATE TABLE promotion_decisions (
    promotion_id uuid NOT NULL REFERENCES promotion_requests (id) ON DELETE CASCADE,
    approver uuid NOT NULL, approve boolean NOT NULL, comment text NOT NULL DEFAULT '' CHECK (length(comment) <= 1000),
    counted boolean NOT NULL, at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (promotion_id, approver)
);
-- Requests and their rows are kept 365 days after finish (a promotion is a release record).
```

Retention summary: `event_outbox` 30 days (partition granularity), `webhook_deliveries` 30 days, `email_deliveries` 30 days, `replication_*` 90 days, `promotion_*` 365 days, `registries`, `webhooks`, `replication_rules` until deleted.

Expand and contract. All five migrations are expand only (`repositories.registry_id` is a nullable column). `ProxyConfig` resolution is done by the repository port: when `registry_id` is set, `remote_url` and `auth` are read from the registry row, so format-core never sees the difference. An N-1 binary does not know the column and reads the inline `proxy_config`, so the registry port keeps that inline copy current: creating a binding and every registry update rewrite the copy (including the re-sealed credential) of all bound proxy repositories in the same transaction. Nothing is gated for that reason. Contract step for a later release: drop the inline copy once no N-1 binary exists. No gate is needed for webhooks, replication or promotion either: nothing an old pod does depends on them.

### 6.d Jobs

| Job | What it does | Idempotency and safety | Metrics |
|---|---|---|---|
| outbox consumer `webhooks` | reads a batch, finds `WebhookRepo::matching` per event, and `create_for_events` (rows plus cursor in one transaction). Pull events only exist while a webhook subscribes | the unique `(webhook, event)` index plus the cursor compare and set make a duplicate batch harmless | `outbox_lag_seconds{consumer}`, `outbox_events_total{kind}` |
| `webhook_delivery` (node daemon, `notify.webhook_workers`, default 8) | `claim` a due delivery, build the body, sign, send through `Purpose::Webhook` egress, `finish`. Retry delays are `10 s x 6^(attempt-1)` capped at 2 hours with plus or minus 20 percent jitter: 10 s, 1 min, 6 min, 36 min, 2 h; after `retry_max` attempts the delivery is `dead`. A 2xx is success; any other status, a redirect (never followed) and every transport error are failures. A success resets `consecutive_failures`; a dead delivery increments it and at `webhooks.suspend_after_failures` (default 100) the webhook is suspended (audit row, `RustryxWebhookBacklog` context) | at least once; every attempt is re-signed with a fresh timestamp; `X-Rustryx-Delivery` is stable across attempts so a receiver can deduplicate; a per host cap of `notify.webhook_per_host` (4) and the global worker count bound the load a webhook can cause | `webhook_deliveries_total{result}`, `webhook_delivery_duration_seconds`, `webhook_deliveries_pending` (gauge), `webhook_suspended_total` |
| outbox consumer `email` | resolves recipients (explicit addresses, project role members, token owner) and `NotificationRepo::create_for_events` | duplicates within 10 minutes become `Suppressed` | `outbox_lag_seconds` |
| `email_delivery` (node daemon) | sends one e-mail at a time per node through the SMTP settings (section 8), plain text only, subject and addresses sanitized (no CR or LF). Retry 1 min, 10 min, 1 h, then `dead`. `notify.email_max_per_hour` (default 200) is a global cap: beyond it deliveries wait | at least once; no SMTP configured means rows stay `Pending`, never `Sent` | `email_deliveries_total{result}` |
| outbox consumer `replication` | for each event of a repository in scope of an enabled `event` rule that matches the filter: `create_event_tasks` with `available_at = occurred_at + event_settle_secs`; a referrer push for an already replicated subject creates a `copy_referrers` task | rows and cursor in one transaction | `outbox_lag_seconds` |
| `replication` (task per scheduled or manual rule) | `start_execution` (the unique index refuses a second active run), enumerates the matching items (push: local components through the OCI catalog port; pull: the remote catalog and tags through `RegistryClient`), `add_tasks` in batches of 1000, then waits for completion with `settle_executions` | enumeration is repeatable: an item already present at the destination with the same digest becomes `skipped` | `replication_executions_running` |
| `replication_worker` (node daemon, `replication.workers`, default 4) | `claim_task`, copy (below), `finish_task`. Retry 1 min, 5 min, 15 min, at most 3 attempts, then `failed` | a task is idempotent (blobs are checked with `HEAD` before upload, a manifest `PUT` of the same digest is a no-op); a lost lease stops the worker | `replication_tasks_total{direction,result}`, `replication_bytes_total{direction}`, `replication_task_duration_seconds` |
| `registry_probe` (every 15 min) | `ping` every registry through `Purpose::Registry` egress, `record_test` | read only | `registry_up` is not a series (cardinality); the status check lists failing registries |
| `promotion_execute` (queue `promotion`, dedupe key the request id) | the executor of section 6.f | resumable: items already `Done` are skipped on a restart | `promotion_requests_total{status}` |
| `webhook_purge`, `outbox_purge`, `task_purge` | drop partitions and old rows per the retention above | idempotent | standard |

**Copy algorithm of a replication task** (OCI first; non OCI content replication is `later`, `PRX-22`):

1. Resolve the subject. For an index, copy every child manifest first, then the index. Each manifest is read with a 4 MiB cap.
2. For each descriptor (config and layers): `blob_exists` at the destination; if missing, stream it from the source to the destination through the bandwidth limiter (the rule's limit, and `replication.default_bandwidth_kbps` when none), computing SHA-256 while streaming. A digest or size that does not match the descriptor aborts the task with `digest_mismatch`; on a push the partial upload is abandoned (the remote discards it), on a pull the staged local blob is aborted (nothing is published).
3. `put_manifest` last, so a destination never shows a manifest whose blobs are missing. With `override_existing = false` an existing tag with another digest is `skipped`; with true it is replaced, **subject to the destination's own immutability**: a Rustryx destination answers `DENIED` and the task fails with `immutable`, a local destination refuses through `publish_with`.
4. With `copy_referrers`, list the subject's referrers at the source and copy each the same way after the subject (`PRX-19`); signatures and SBOMs travel with the artifact. Documented limit: a third party target may not link them to the subject.
5. A `Delete` task (event rules with `replicate_deletion`) deletes the manifest at the destination; the source side skips an item that is immutable or held, and the destination refuses an immutable tag.
6. Pull direction writes to the local repository through the same publish path as `docker push` (write policy, immutability, quota, read only mode, events); the actor is a synthetic principal `system:replication:<rule id>` with write on that scope only. The pull gate and the trust policy apply to the **source read** when the source is local (push direction): blocked or unsigned content is not replicated and the task is `skipped` with `blocked_by_gate`.
7. Protocol versioning (`PRX-21`): for a `rustryx` registry, `GET <url>/api/v1/replication/protocol` returns `{"protocol": 1, "min_compatible": 1}`; the executor refuses a peer whose range excludes its own (`incompatible_peer`). Generic OCI peers need no negotiation.

### 6.e Authorization

| Operation | Requires |
|---|---|
| registries (create, edit, test, delete) | `App(Registries, Write)`, phase 1 `nx-admin`; read of names for a proxy picker: `Authenticated` with `Repo(admin)` of that repository |
| replication rules, executions, stop, run now | `App(Replication, Write)`, phase 1 `nx-admin`; read: `App(Replication, Read)` |
| `replicate_deletion` on a rule | `nx-admin`, audited |
| system webhook | `App(Webhooks, Write)`, phase 1 `nx-admin` |
| project webhook | `Project(ManageWebhooks)` |
| repository webhook | `Project(ManageWebhooks)` when the repository has a project, else `nx-admin` |
| deliveries, test delivery, rotate secret | the same as editing that webhook |
| notification subscription | the same scope rules as webhooks; `ProjectRole` recipients only on project scoped subscriptions |
| request a promotion or copy | `Repo(read)` on the source and `Repo(write)` on the target (project roles give both); a direct copy into a target that requires approval is refused (409 `approval_required`) |
| approve or reject | `Project(ApprovePromotion)` of the **target** repository's project, or `nx-admin` when the target has no project; a counted approval also needs approver is not the requester and not an author of a selected component |
| set promotion target rules | `Project(ManagePolicies)` of the target's project, or `nx-admin` |
| executor | a synthetic `system:promotion:<id>` principal with write on the target and read on the source |

### 6.f Events, audit and the promotion flow

| Event | Outbox | Audit `action` and kind | Data |
|---|---|---|---|
| webhook, registry, rule, subscription config | none | `webhook.create`, `.update`, `.delete`, `.rotate_secret`, `.suspend`, `registry.create`, `.update`, `.delete`, `replication_rule.*`, `IntegrationChange` | name, kind, scope, URL host and path (never the query), events; no secrets |
| replication run end | `replication.finished` (in `settle_executions`) | `replication.run`, `LifecycleAction` | rule name, status, counts, trigger |
| replication task refused by policy | none | `replication.refuse`, `SecurityDecision` | rule, item, reason code |
| egress refusal (SSRF) | none | `egress.reject`, `SecurityDecision` | purpose, reason code, host (no address resolution detail) |
| promotion | `promotion.requested`, `.approved`, `.rejected`, `.completed` | `promotion.request`, `.approve`, `.reject`, `.execute`, `.cancel`, `PromotionChange` | who, what (count and first 20 coordinates), from, to, checks, decisions, `counted` flags |
| e-mail | none | none (metrics only) | |

**Promotion flow (DSO-28).**

1. *Request.* The requester names source, target and a selector (at most 10 000 components). The service snapshots the target's `PromotionTarget` (required approvals, checks) into the request. Items are components that exist in the source and are not referrers.
2. *Pre check at request time* (advisory): the checks are evaluated and shown; a failing component is listed, and the request is still stored so approvers see why.
3. *Approval.* Approvers other than the requester and the authors of selected components are counted. When counted approvals reach `required_approvals` the request becomes `Approved` and the executor job is enqueued in the same transaction. A rejection ends the request. A pending request expires after 14 days.
4. *Execution* (`promotion_execute`): a `Promotion` hold is taken on the source components for the duration. For each component the checks are **evaluated again** at execution time (a scan can have changed since approval: `ScanBelowThreshold` through `ScanRepo::effective_summary`, `SignatureValid` through the verifier, `SbomPresent` through the referrers list); a failing component is `Skipped(check_failed)`. Then copy: same blob store gives `AssetRepo::link` (a metadata only copy, quota counted once in the target scope), different stores stream the blob with a checksum check and use `publish_with`; OCI uses manifest and blob linking in the same store, tags and referrers follow. A target item that already exists with the same digest is `Done`; with a different digest it is `Skipped(target_exists)` unless the target write policy allows replacement, and an immutable target is `Skipped(immutable)`.
5. *Move* deletes the source component only after the target commit of that item, through `delete_checked`; a source that is immutable, held or in a release repository is left in place and the item records `Done` with the delete skipped reason in `reason`.
6. *Finish*: the hold is released, one `promotion.completed` event and one audit row summarize the run. Auto-promotion rules (promote when checks pass, with no human) are not in this stream (open question 4 in section 13).

### 6.g Crates that change

| Crate | Change |
|---|---|
| `rustryx-core` | `registry.rs`, `replication.rs`, `webhook.rs`, `notification.rs`, `promotion.rs`, `ports/distribution.rs`, `ProxyConfig` fields, `ContentEvent` consumer wiring |
| `rustryx-notify` (new) | `sign` and `verify`, the dispatcher (`webhook_delivery`, `email_delivery`), outbox consumers `webhooks` and `email`, payload builders (default JSON and CloudEvents structured mode), the e-mail sender and templates (plain text), a reference receiver snippet for the docs |
| `rustryx-replication` (new) | `RegistryClient` over egress, `BandwidthLimiter` (token bucket, no shared state, per task), executor, workers, outbox consumer `replication`, `PromotionExecutor`, protocol negotiation |
| `rustryx-egress` | streaming `POST`, `PATCH`, `PUT` bodies, `Purpose::{Registry, Webhook, Smtp}`, a body size cap per purpose |
| `rustryx-db` | migrations 0080 to 0084, repositories, `ProxyConfig` resolution through `registry_id`, partition functions for deliveries |
| `rustryx-tasks` | catalog rows of this stream |
| `rustryx-api` | `/api/v1/registries`, `/replication-rules`, `/replication-executions` (+ tasks, logs), `/webhooks`, `/webhook-deliveries`, `/notification-subscriptions`, `/promotions` (+ approvals, items), `/promotion-targets`, `/api/v1/replication/protocol` (public, no data), the `copy` convenience route `POST /promotions` with `mode=copy` |
| `rustryx-format-core` | emits `ContentEvent` consumers for pull events through the outbox buffer; `OciCatalogRepo` and `OciTagRepo` use (section 4.g contract) |
| `rustryx-config` | `egress.registries.*`, `egress.webhooks.*`, `egress.smtp.*`, `notify.webhook_workers`, `notify.webhook_per_host`, `notify.webhook_max_response_bytes` (65536), `notify.email_max_per_hour` (200), `replication.workers` (4), `replication.task_timeout_secs` (3600), `replication.blob_idle_timeout_secs` (60), `replication.default_bandwidth_kbps` (unset), `webhooks.suspend_after_failures` (100) |
| `rustryx-testkit` | in memory repositories, `FakeRegistry` (an in process distribution server for the replication tests), `FakeReceiver` for webhook tests |

Webhook request, for the docs and for receivers:

```text
POST <url>
Content-Type: application/json            (CloudEvents kind: application/cloudevents+json)
User-Agent: rustryx-webhook/<version>
X-Rustryx-Event: scan.completed
X-Rustryx-Delivery: <delivery uuid, stable across attempts>
X-Rustryx-Timestamp: <unix seconds, per attempt>
X-Rustryx-Signature: sha256=<hex HMAC-SHA256(secret, "<timestamp>." + raw body)>
Authorization: <auth_header, when configured>

{"id":"<event id>","type":"scan.completed","version":1,"occurred_at":"2026-10-24T10:00:00Z",
 "scope":{"project":"payments","repository":"payments"},"actor":{"name":"ci","kind":"robot"},"data":{...}}
```

The signature header name and the `sha256=` prefix are those of DSO-32; the timestamp inside the signed bytes is SEC-38. SEC-38 is the stricter rule and wins where they differ (a body only signature has no replay protection); receivers verify with the constant time compare of the reference snippet and reject a timestamp more than 300 seconds old.

### 6.h Negative requirements

| Requirement | Test |
|---|---|
| Webhook, registry, replication and scanner targets pass the whole SSRF class list: loopback, private, link local, metadata, CGNAT, multicast, unspecified, mapped, NAT64, 6to4, alternate notations, user info, `file:` and other schemes | `egress::must_not_*` (reused from format-core, run per purpose), `notify::must_not_deliver_to_an_internal_address`, `replication::must_not_replicate_to_an_internal_registry`, `api::must_not_accept_an_internal_webhook_url` |
| The target is re-validated on every delivery, not only at creation; one DNS lookup, the checked address is the connected one | `notify::must_not_follow_a_dns_rebind_at_delivery`, `egress::resolves_once_per_call` |
| A webhook never follows a redirect, including to an internal address | `notify::must_not_follow_a_redirect` |
| No secret in any response, list, log line, audit row or error: webhook secret and auth header, registry password, SMTP password | `api::must_not_return_a_webhook_secret_after_creation`, `..._a_registry_password`, `notify::must_not_log_the_signing_secret`, `..._the_auth_header` |
| The signature covers the timestamp and the body and is verified in constant time, with a 5 minute window | `notify::must_not_verify_a_modified_body`, `..._a_replayed_timestamp`, `..._compare_signatures_with_equality` (a source scan for `==` on signature types), `..._accept_a_stale_timestamp` |
| A webhook without a secret cannot exist; the secret is generated with at least 256 bits when the caller gives none | `db::must_not_store_a_webhook_without_a_secret`, `api::must_not_accept_a_short_supplied_secret` |
| An event exists if and only if its state change committed | `db::must_not_leave_an_event_after_a_rollback`, `..._lose_an_event_after_a_commit` |
| A consumer never skips an event and never handles one twice across nodes | `db::must_not_skip_a_late_committing_event`, `notify::must_not_deliver_twice_after_a_lost_lock` |
| Pull events are not written unless subscribed | `format_core::must_not_write_pull_events_without_a_subscriber` |
| A webhook cannot subscribe to the internal event kinds, nor have more than 32 events, nor exceed 20 per scope | `api::must_not_subscribe_to_internal_events` |
| A slow or hostile receiver cannot hold a worker: timeout, response size cap, per host cap | `notify::must_not_wait_beyond_the_timeout`, `..._read_beyond_the_response_cap`, `..._exceed_the_per_host_cap` |
| E-mail headers cannot be injected; no HTML | `notify::must_not_allow_header_injection_in_subject_or_address` |
| E-mail is never marked sent without an SMTP server, and the cap holds | `notify::must_not_mark_sent_without_smtp`, `..._exceed_the_hourly_cap` |
| Replication never overrides an immutable tag on the destination, never replicates content the gate or trust policy refuses, never deletes remote without the flag | `replication::must_not_override_an_immutable_tag`, `..._replicate_blocked_content`, `..._delete_remote_without_the_flag` |
| A replicated blob with a wrong digest is neither stored nor published | `replication::must_not_publish_a_blob_with_a_wrong_digest` |
| One active execution per rule, across nodes | `db::must_not_start_two_executions_of_one_rule` |
| A rule cannot change while an execution runs; a registry in use cannot be deleted | `db::must_not_update_a_running_rule`, `..._delete_a_registry_in_use` |
| A peer of an incompatible protocol is refused | `replication::must_not_talk_to_an_incompatible_peer` |
| A promotion needs the required counted approvals; the requester and the authors do not count | `db::must_not_count_the_requesters_approval`, `..._an_authors_approval`, `api::must_not_run_a_promotion_without_approval` |
| Checks are evaluated again at execution | `replication::must_not_promote_a_component_that_fails_a_check_at_execution` |
| A direct copy into an approval-required target is refused | `api::must_not_copy_directly_into_a_release_target` |
| A move never deletes the source before the target committed, nor an immutable or held source | `replication::must_not_delete_the_source_before_the_target_commit`, `..._an_immutable_source` |
| Cross-project isolation: a promotion or copy needs read on the source and write on the target, and a project scoped webhook never receives another project's events | `api::must_not_copy_without_read_on_the_source`, `notify::must_not_deliver_another_projects_event` |

---

## 7. W2-F: storage

Capabilities: `BLOB-04` to `BLOB-07`, `BLOB-10`, `BLOB-11`, `BLOB-16`, `BLOB-19`. Decisions of parity section 12.9 and of the open blob gaps (gaps 5 to 11, 28, 29) are made here. The crate that carries the work is `rustryx-blob`; the stream has the fewest dependencies on the others and can start as soon as Core revision 2 lands.

Sub-steps: **F1** group store, member records, placement, the registry trait change, group conversion; **F2** Azure, S3 credential and encryption options (after spikes S5 and S6), presigned redirects; **F3** `verify_blobs`, member removal task, move ledger.

### 7.a Entities and enums

```rust
// core: crates/rustryx-core/src/storage.rs  (new file)
/// How a group chooses the member that takes a new blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillPolicy {
    /// Even spread. Implemented as `hash(key) mod n` over the eligible members in position order, not as a shared counter:
    /// it needs no coordination between nodes and gives the same member for the same key (ADR-W2-27).
    RoundRobin,
    /// The first eligible member in position order.
    WriteToFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGroupConfig { pub fill_policy: FillPolicy }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState { Active, Draining }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStoreMember { pub group_id: BlobStoreId, pub member_id: BlobStoreId, pub position: u16, pub state: MemberState }

/// Ledger of a blob that moved between member stores. The old object is deleted only after `cleanup_after`, which is at
/// least twice `limits.request_timeout_secs`, so a reader that resolved the old store before the switch can finish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobMove { pub blob_id: BlobId, pub from_store: BlobStoreId, pub to_store: BlobStoreId, pub key: String,
                      pub cleaned: bool, pub moved_at: OffsetDateTime, pub cleanup_after: OffsetDateTime }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode { Report, Repair }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyFindingKind {
    /// A blob row whose object is not in the store: data loss, critical.
    Missing,
    /// An object with no row older than the grace age: a leak (also the lost S3 commit response of gap 28).
    Orphan,
    ChecksumMismatch,
    SizeMismatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyCounts { pub listed_objects: u64, pub rows_checked: u64, pub missing: u64, pub orphans: u64,
                          pub checksum_checked: u64, pub checksum_mismatch: u64, pub size_mismatch: u64, pub repaired: u64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VerifyRun {
    pub id: VerifyRunId,
    pub blob_store_id: BlobStoreId,
    pub mode: VerifyMode,
    pub status: TaskRunStatus,
    pub counts: VerifyCounts,
    /// Share of rows whose bytes are re-read and re-hashed, 0 to 100. Default 0.1.
    pub checksum_sample_percent: f32,
    pub task_run_id: Option<TaskRunId>,
    pub requested_by: Option<UserId>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

pub struct VerifyFinding { pub run_id: VerifyRunId, pub seq: u32, pub kind: VerifyFindingKind, pub blob_id: Option<BlobId>,
                           pub key: String, pub detail: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStoreStats { pub blob_store_id: BlobStoreId, pub used_bytes: u64, pub blob_count: u64,
                            pub free_bytes: Option<u64>, pub total_bytes: Option<u64>, pub refreshed_at: OffsetDateTime }
```

`BlobStoreDef.group` (section 1.3.3) is `Some(BlobGroupConfig)` exactly for `kind = Group`; the members are rows of `blob_store_members`, not JSON, so they have foreign keys. `BlobStoreDef.config` of a group is `{"fill_policy": "round_robin"}`.

Store definition fields added to `BlobStoreDef.config` (JSON, unknown fields still rejected, secrets only as references, `rustryx-blob` parses and validates):

| Store | Field | Default | Meaning |
|---|---|---|---|
| S3 | `encryption` | `{"mode":"none"}` | `mode` is `none`, `sse_s3` or `sse_kms`; `kms_key_id` is required for `sse_kms` and must be a full key id or ARN (`BLOB-05`) |
| S3 | `max_connections` | 32 | HTTP connection pool size, 1 to 256 (`BLOB-06`) |
| S3 | `role_arn`, `role_session_name`, `external_id_ref` | none | assume role on top of static or ambient credentials, only if spike S6 shows `object_store` cannot do it (then the credential provider comes from `aws-config`, which is already an accepted alternative of 02-rust-engineering) |
| S3, Azure | `presign` | `{"enabled":false,"ttl_secs":300}` | presigned redirects, `ttl_secs` 1 to 900 (`BLOB-16`) |
| Azure | `account_name`, `container` | required | |
| Azure | `endpoint` | the public cloud endpoint | `https` unless `allow_http` (Azurite) |
| Azure | `prefix`, `part_size_mib` (16, 1 to 100), `max_concurrent_uploads` (32), `allow_http` (false) | | same meaning as S3 |
| Azure | `auth` | required | an object `{"kind": ..., "secret_ref": "NAME"}` where `kind` is one of `shared_key`, `sas`, `managed_identity`, `workload_identity`, `ambient`; the account key or SAS token is only ever a reference resolved by `SecretResolver` |

### 7.b Ports

```rust
// core: crates/rustryx-core/src/ports/storage.rs  (new file)
// BlobRepo further additions (section 1.3.8 has insert, list_unreferenced_in, delete_if_unreferenced):
//     /// Switches a blob to another store with compare and set on the current store. `false` when it already moved.
//     async fn move_blob(&self, id: BlobId, from: BlobStoreId, to: BlobStoreId) -> Result<bool, StoreError>;
//     /// Rows of a store after `after_id`, in id order, for the move task and for verify. Referenced rows only when `referenced_only`.
//     async fn list_page_by_store(&self, store: BlobStoreId, after: Option<BlobId>, limit: u32, referenced_only: bool)
//         -> Result<Vec<BlobRecord>, StoreError>;

#[async_trait]
pub trait BlobStoreMemberRepo: Send + Sync {
    async fn members(&self, group: BlobStoreId) -> Result<Vec<BlobStoreMember>, StoreError>;     // position order
    /// `Conflict` when the store is a group, is already a member of a group, or equals the group (no nesting).
    async fn add_member(&self, group: BlobStoreId, member: BlobStoreId, position: u16) -> Result<(), StoreError>;
    async fn set_state(&self, group: BlobStoreId, member: BlobStoreId, state: MemberState) -> Result<(), StoreError>;
    /// `Conflict` while any blob row still names the member, or when it would leave a group that repositories use without members.
    async fn remove_member(&self, group: BlobStoreId, member: BlobStoreId) -> Result<(), StoreError>;
    /// One transaction: inserts `group` (kind `Group`), makes `existing` its first member and repoints every repository from
    /// `existing` to the group. Blob rows are not touched, so it is safe at any time. Publishes a bus event.
    async fn convert_to_group(&self, existing: BlobStoreId, group: &BlobStoreDef) -> Result<(), StoreError>;
    /// The group a store belongs to, if any.
    async fn group_of(&self, member: BlobStoreId) -> Result<Option<BlobStoreId>, StoreError>;
}

#[async_trait]
pub trait BlobMoveRepo: Send + Sync {
    /// Upserts the ledger row of a switched blob (key `(blob_id, from_store)`, so two moves inside the cleanup window do not
    /// lose track of the first old object).
    async fn record(&self, mv: &BlobMove) -> Result<(), StoreError>;
    async fn due_for_cleanup(&self, now: OffsetDateTime, limit: u32) -> Result<Vec<BlobMove>, StoreError>;
    async fn mark_cleaned(&self, blob: BlobId, from: BlobStoreId) -> Result<(), StoreError>;
}

#[async_trait]
pub trait VerifyRunRepo: Send + Sync {
    async fn create(&self, run: &VerifyRun) -> Result<(), StoreError>;
    async fn get(&self, id: VerifyRunId) -> Result<Option<VerifyRun>, StoreError>;
    async fn list(&self, store: Option<BlobStoreId>, page: &PageRequest) -> Result<Page<VerifyRun>, StoreError>;
    async fn update(&self, id: VerifyRunId, counts: &VerifyCounts) -> Result<(), StoreError>;
    async fn finish(&self, id: VerifyRunId, status: TaskRunStatus, counts: &VerifyCounts, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Stages the keys listed from the store (unlogged table, one run). Idempotent per `(run, key)`.
    async fn stage_listing(&self, run: VerifyRunId, objects: &[(String, u64, OffsetDateTime)]) -> Result<(), StoreError>;
    /// Anti joins of the staged listing against `blobs` of the store: rows with no object, and objects with no row older than `grace`.
    async fn missing(&self, run: VerifyRunId, store: BlobStoreId, after: Option<BlobId>, limit: u32) -> Result<Vec<BlobRecord>, StoreError>;
    async fn orphans(&self, run: VerifyRunId, store: BlobStoreId, grace_before: OffsetDateTime, after: Option<String>, limit: u32)
        -> Result<Vec<String>, StoreError>;
    async fn size_mismatches(&self, run: VerifyRunId, store: BlobStoreId, after: Option<BlobId>, limit: u32) -> Result<Vec<(BlobRecord, u64)>, StoreError>;
    async fn save_findings(&self, findings: &[VerifyFinding]) -> Result<(), StoreError>;       // at most 10 000 per run
    async fn list_findings(&self, run: VerifyRunId, page: &PageRequest) -> Result<Page<VerifyFinding>, StoreError>;
    async fn drop_listing(&self, run: VerifyRunId) -> Result<(), StoreError>;
}

#[async_trait]
pub trait BlobStoreStatsRepo: Send + Sync {          // written by `blob_store_stats` (section 4.d)
    async fn upsert(&self, stats: &BlobStoreStats) -> Result<(), StoreError>;
    async fn get(&self, store: BlobStoreId) -> Result<Option<BlobStoreStats>, StoreError>;
    async fn list(&self) -> Result<Vec<BlobStoreStats>, StoreError>;
}
```

Registry trait in `rustryx-format-core` (this is where group reads stay correct):

```rust
// crate: rustryx-format-core
#[async_trait]
pub trait BlobStoreRegistry: Send + Sync {
    /// The store that takes a NEW write for this repository: for a group backed repository, the group adapter.
    async fn store_for(&self, repository: &Repository) -> Result<Arc<dyn BlobStore>, BlobError>;
    /// The store by id (a member, a plain store). Required.
    async fn store_by_id(&self, id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError>;
    /// The store that holds an EXISTING blob: the member named in the blob row, never the group. Reads, ranges, deletes, the
    /// scan export and replication reads all use it, so no read ever probes members. Provided.
    async fn store_for_blob(&self, blob: &BlobRecord) -> Result<Arc<dyn BlobStore>, BlobError> { self.store_by_id(blob.blob_store_id).await }
}
```

### 7.c PostgreSQL

Migrations `0100_blob_groups.sql`, `0101_blob_moves.sql`, `0102_verify.sql`. (`blob_store_stats` and `soft_quota` are in 0040 of W2-C.)

```sql
-- 0100: widen the kind check (value widening: written only after the BlobGroupsAndAzure gate opens) and add members
ALTER TABLE blob_stores DROP CONSTRAINT blob_stores_kind_check;
ALTER TABLE blob_stores ADD CONSTRAINT blob_stores_kind_check CHECK (kind IN ('local', 's3', 'azure', 'group'));

CREATE TABLE blob_store_members (
    group_id  uuid     NOT NULL REFERENCES blob_stores (id) ON DELETE CASCADE,
    member_id uuid     NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    position  smallint NOT NULL CHECK (position >= 0),
    state     text     NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'draining')),
    PRIMARY KEY (group_id, member_id),
    CONSTRAINT uq_member_position UNIQUE (group_id, position) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT uq_member_single_group UNIQUE (member_id),          -- a store belongs to at most one group
    CONSTRAINT ck_member_not_self CHECK (group_id <> member_id)
);
-- Trigger members_shape (RX008): group_id must be of kind 'group', member_id must NOT be of kind 'group' (no nesting).
-- Trigger members_remove_guard: removing a member while any blobs row has blob_store_id = member_id is refused.

-- 0101: ledger of moved blobs
CREATE TABLE blob_moves (
    blob_id       uuid        NOT NULL REFERENCES blobs (id) ON DELETE CASCADE,
    from_store    uuid        NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    to_store      uuid        NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    key           text        NOT NULL,
    cleaned       boolean     NOT NULL DEFAULT false,
    moved_at      timestamptz NOT NULL DEFAULT now(),
    cleanup_after timestamptz NOT NULL,
    PRIMARY KEY (blob_id, from_store)
);
CREATE INDEX ix_blob_moves_cleanup ON blob_moves (cleanup_after) WHERE NOT cleaned;

-- 0102: verification
CREATE TABLE verify_runs (
    id uuid PRIMARY KEY, blob_store_id uuid NOT NULL REFERENCES blob_stores (id) ON DELETE CASCADE,
    mode text NOT NULL CHECK (mode IN ('report', 'repair')), status text NOT NULL,
    counts jsonb NOT NULL DEFAULT '{}'::jsonb, checksum_sample_percent real NOT NULL DEFAULT 0.1 CHECK (checksum_sample_percent BETWEEN 0 AND 100),
    task_run_id uuid, requested_by uuid, started_at timestamptz, finished_at timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE verify_findings (
    run_id uuid NOT NULL REFERENCES verify_runs (id) ON DELETE CASCADE, seq integer NOT NULL,
    kind text NOT NULL CHECK (kind IN ('missing', 'orphan', 'checksum_mismatch', 'size_mismatch')),
    blob_id uuid, key text NOT NULL, detail text NOT NULL CHECK (length(detail) <= 200),
    PRIMARY KEY (run_id, seq)
);
CREATE UNLOGGED TABLE verify_listing (
    run_id uuid NOT NULL, key text NOT NULL, size_bytes bigint NOT NULL, modified timestamptz NOT NULL,
    PRIMARY KEY (run_id, key)
);
-- verify_runs and findings are kept 90 days; verify_listing rows are dropped by the run itself and by task_purge if a run died.
```

An unlogged listing table is deliberate: it is scratch data for one run, it is lost on a crash (the run is simply started again), and it avoids WAL for tens or hundreds of millions of keys. It is not replicated, so `verify_blobs` runs on the primary.

Expand and contract. All three migrations are expand. The widened `kind` check is a value widening: the first `azure` or `group` store is created only when `BlobGroupsAndAzure` is open, because an N-1 pod that loads a store definition of an unknown kind fails to build the store and answers 503 for every repository on it. Reads of group content by an N-1 pod would work only if it resolved the member from the blob row, which it does not (it uses `store_for(&Repository)`), which is the reason the gate exists. No contract step.

### 7.d Jobs

| Job | What it does | Idempotency and safety | Metrics |
|---|---|---|---|
| `verify_blobs` (per store, weekly, report mode) | 1 stage the store's committed keys with `BlobStore::list_keys` into `verify_listing` (the `_staging/` and `_uploads/` prefixes are excluded and handled by the staging sweep); 2 anti join both ways: **missing** (rows with no object, critical), **orphans** (objects with no row and `modified` older than `2 x gc.min_age_secs`), **size mismatches**; 3 sample `checksum_sample_percent` of the rows by `hashtext(id)`, stream the object through SHA-256 under `verify.checksum_bandwidth_mbps` (default 50) and compare with the row; 4 write findings (capped at 10 000, counts exact), drop the listing. A store without `lists_keys` finishes `failed` with `unsupported` instead of reporting nothing | report mode changes nothing. **Repair mode** (manual, `nx-admin`, typed confirmation) deletes only orphan objects, one at a time, re-checking that no row names the key; it never deletes a row and never touches a missing object, because a missing object is evidence of data loss that needs a restore, not a tidy up | `verify_blobs_findings_total{kind}`, `verify_blobs_duration_seconds`, standard |
| `blob_member_move` (manual, per group) | for a member in `draining` state: pages the rows of that member (`list_page_by_store`, referenced first), for each blob copies the object to the member chosen by the group policy under a bandwidth cap, verifies that the SHA-256 computed during the write equals the row's `sha256`, then `move_blob` (compare and set on the old store) and `BlobMoveRepo::record`. When no row names the member, waits `limits.request_timeout_secs` and rescans once (a publish that began before the drain flag may still commit to the member), then `remove_member` | resumable: every blob is an independent compare and set; a crash leaves either the old state or the switched state, plus at worst an extra copy that `verify_blobs` reports as an orphan; the old object is not deleted here | `blob_moves_total{result}`, `blob_move_bytes_total` |
| `blob_move_cleanup` (every 10 min) | deletes the old object of every ledger row past `cleanup_after` (a missing object counts as done) and marks it cleaned | `cleanup_after` is validated to be at least twice `limits.request_timeout_secs`, so an in-flight read of the old object finishes first | `blob_moves_total{result="cleaned"}` |
| (daemon) group member health | each node probes every member with `BlobStore::health` every 10 s (cached) and uses the result for placement; there is no shared state and no job | | `blob_store_member_state{group,member}` (0 active, 1 draining, 2 offline) |

**Placement rules of the group adapter** (the `BlobStore` that `store_for` returns for a group backed repository):

1. Eligible members: `Active` state, healthy within the last probe, and not at their hard `quota_bytes`. Among eligible members, those **under their soft quota** are preferred. If every eligible member is over its soft quota the write still goes to one of them, because a soft quota alerts and never blocks (parity 12.9, 11.5). If no member is eligible the write fails with `InsufficientStorage` (507).
2. `FillPolicy::RoundRobin` picks `hash(key) mod n` over the preferred set in position order; `WriteToFirst` picks the first. The chosen member is returned in `BlobCommit.placement`, the pipeline stores it as `BlobRecord.blob_store_id`, so **every blob row names the concrete member** and reads never probe.
3. `get`, `head`, `delete` and `list_*` on the group adapter itself are refused with `BlobError::Unsupported("group")` on the hot path; callers resolve the member through `store_for_blob`. `list_keys` on a group returns the union of its members and exists for tooling only.
4. Used bytes of a group are the sum over its members (`BlobRepo::used_bytes_by_store` resolves a group to its members); `gc:<member>` locks and sweeps are per member store.

**Convert to group** (`BLOB-11`): one transaction creates the group, makes the existing store its member at position 0, and repoints the repositories; blob rows are unchanged, so it is safe and instant. It stays irreversible in the UI (there is no "ungroup" call); an operator can still undo it by SQL because the data never moved. **Remove a member**: set it `draining` (no new writes), run `blob_member_move`, then it is removed when empty. A group cannot contain a group, a store cannot be in two groups.

**Presigned redirects** (`BLOB-16`), decided as follows. Off by default, per store (`presign.enabled`). A download is redirected with `307` to `BlobStore::presign_get` only when **all** hold: the store `can_presign`; the format is not `pub` (the Authorization-prefix rule of 01-architecture 1.6.1: a redirect leaves the hosted URL prefix); the asset is committed (never while a proxy cache fill is still running, the cache fill's tee path serves the client itself); authorization and the pull gate have already allowed the request; the request is a plain `GET` (a `HEAD`, a conditional request that answers `304`, and a refused range are handled by the node, so no redirect is wasted); and for OCI the path is a blob (manifests are never redirected). The URL is a bearer capability: it is never logged (the redactor scrubs `X-Amz-Signature` and the Azure `sig` parameter), never audited, sent with `Cache-Control: no-store` and `Referrer-Policy: no-referrer`, valid at most `ttl_secs` (default 300, maximum 900), and carries `response-content-type` and, for active content types, `response-content-disposition: attachment` so the CSP sandbox that the node adds on direct serving (SEC-35) is not lost. The download is counted (`DownloadSource::Client`) at redirect time. If `presign_get` fails the node serves the bytes itself.

### 7.e Authorization

| Operation | Requires |
|---|---|
| list stores, usage, members | `App(Blobstores, Read)`, phase 1 `nx-admin` |
| create, update, delete, convert, add or drain a member, enable presign | `App(Blobstores, Write)`, phase 1 `nx-admin` |
| verify runs (report) | `App(Blobstores, Read)`; repair mode: `nx-admin` plus typed confirmation |
| the presigned redirect | the normal read authorization of the repository; nothing new |

### 7.f Events and audit

No outbox events. Audit rows: `blobstore.create`, `.update`, `.delete`, `.convert_group`, `.member_add`, `.member_drain`, `.member_remove`, `.presign_change` (`ConfigChange`, no credentials, store config with secret references shown as references), `verify.run`, `verify.repair` (`LifecycleAction`, counts and the first 20 keys), `blob.move_batch` (`LifecycleAction`, count and bytes per batch, not per blob).

### 7.g Crates that change

| Crate | Change |
|---|---|
| `rustryx-core` | `storage.rs`, `ports/storage.rs`, `BlobStoreKind`, `BlobStoreDef`, `BlobStore` provided methods, `BlobCommit.placement`, `BlobError` variants |
| `rustryx-blob` | `GroupBlobStore` adapter and placement, `AzureBlobStore` (through `object_store` with the `azure` feature, same block upload and commit design as S3, uncommitted blocks expire on their own as the S3 lifecycle rule does), S3 encryption, pool and assume role options, `presign_get` for S3 (and Azure after the spike), `list_keys` for local (shard walk), S3 and Azure, `capabilities`, `shutdown` (replaces `CleanupDrain`), the local disk free probe behind `DiskProbe` (closes the SRE-34 open item: `InsufficientStorage` when free space is below `blob.local_min_free_percent`), `store_by_id` in the registry, the metrics `blob_store_ops_total`, `blob_store_bytes_total` that slo.md lists as not yet exported |
| `rustryx-format-core` | `BlobStoreRegistry` trait change, download path uses `store_for_blob`, the presigned redirect decision, `BlobCommit.placement` into `BlobRecord` |
| `rustryx-db` | migrations 0100 to 0102, member, move and verify repositories, `delete_if_unreferenced`, `move_blob`, group aware `used_bytes_by_store`, the kind widening |
| `rustryx-tasks` | `verify_blobs`, `blob_member_move`, `blob_move_cleanup` handlers |
| `rustryx-api` | `/api/v1/blob-stores` (+ `/members`, `/usage`, `/actions/convert-to-group`), `/api/v1/blob-stores/{name}/verify-runs` |
| `rustryx-testkit` | `MemBlobStore` implements `list_keys` and `capabilities`; a `MemGroupStore`; in memory member and verify repositories |

REST resources: `GET/POST /api/v1/blob-stores`, `GET/PUT/DELETE /api/v1/blob-stores/{name}`, `GET /api/v1/blob-stores/{name}/usage`, `GET/POST /api/v1/blob-stores/{name}/members`, `PUT/DELETE /api/v1/blob-stores/{name}/members/{member}`, `POST /api/v1/blob-stores/{name}/actions/convert-to-group`, `POST /api/v1/blob-stores/{name}/verify-runs`, `GET /api/v1/blob-stores/{name}/verify-runs/{id}[/findings]`.

### 7.h Negative requirements

| Requirement | Test |
|---|---|
| A group cannot contain a group; a store cannot be in two groups | `db::must_not_nest_groups`, `..._share_a_member_between_groups` |
| Reads never probe members | `blob::must_not_probe_members_on_read` (a counting store behind each member), `format_core::must_not_read_through_the_group_adapter` |
| A write never goes to an offline, draining or hard full member, and a soft quota never blocks a write | `blob::must_not_write_to_a_draining_member`, `..._an_offline_member`, `..._block_a_write_over_the_soft_quota` |
| The round robin spread is stable per key and even | `blob::must_spread_evenly_and_stably_by_key` (proptest) |
| Moving a blob verifies its checksum before the switch, switches with compare and set, and deletes the old object only after the delay | `tasks::must_not_switch_a_blob_with_a_wrong_checksum`, `db::must_not_move_a_blob_twice_concurrently`, `tasks::must_not_delete_the_old_object_before_the_cleanup_delay` |
| The cleanup delay is at least twice the request timeout, validated at startup | `config::must_not_accept_a_cleanup_delay_below_twice_the_request_timeout` |
| A member that still holds blobs cannot be removed | `db::must_not_remove_a_member_that_holds_blobs` |
| Converting to a group changes no blob row and no data | `db::must_not_touch_blobs_when_converting_to_a_group` |
| Presigned redirects never for `pub`, never during a cache fill, never for manifests, `HEAD` or `304`, never with a TTL above 900 | `format_core::must_not_redirect_pub`, `..._during_a_proxy_fill`, `..._a_manifest`, `..._a_head_request`, `blob::must_not_sign_beyond_the_max_ttl` |
| A presigned URL never appears in logs, audit rows or error text | `observability::must_not_log_a_presigned_url`, `api::must_not_audit_a_presigned_url` |
| `verify_blobs` repair deletes only orphans, never a row, never a missing object's record | `tasks::must_not_repair_a_missing_object`, `..._delete_a_row`, `..._delete_an_object_a_row_names` |
| `verify_blobs` on a store that cannot list keys says so instead of reporting a clean store | `tasks::must_not_report_clean_when_listing_is_unsupported` |
| Azure and S3 credentials are references: never in the config JSON, responses or logs; an unknown field is refused | `blob::must_not_accept_a_credential_in_the_config`, `api::must_not_return_a_blob_store_secret` |
| Local writes are refused below the free space floor and reads and deletes continue | `blob::must_refuse_writes_below_the_free_space_floor`, `..._block_reads_when_the_disk_is_full` |
| A group or Azure store cannot be created while an older node is live | `api::must_not_create_a_group_while_an_older_node_is_live` |

---

## 8. W2-H: platform settings

Capabilities in this document: `OPS-06`, `OPS-14`, `OPS-19`, `OPS-22` (semantics in section 4.d), `PRX-07`, `AUTH-85`, `OPS-24`, and the settings sections that the other streams need (`OPS-25` is answered by ADR-W2-33: typed sections instead of a capability framework). The rest of parity stream W2-H (LDAP multi-server and verify, OIDC admin screen and logout, remote user token, default role, per area application privileges, CLI, backup, export and import, compat migration) is outside this document; the Core revision only reserves `AppArea` and `PrivilegeGrant::Application` for it.

Sub-steps: **H1** the settings framework (port, service, node cache and refresh, schema endpoint) and the trust store; **H2** SMTP and the outbound proxy; **H3** log overrides and the status check registry.

### 8.a Entities and sections

Operator tunable policy lives in the database as typed, versioned, audited sections; the security boundary and bootstrap values stay in file configuration (ADR-W2-24). A section is one JSON document under the key `settings.<section>` in `system_config`, parsed into a typed struct with `deny_unknown_fields`, validated by code, edited through one generic endpoint, and applied by every node within seconds.

```rust
// core: crates/rustryx-core/src/settings.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingsSection {
    Smtp, OutboundProxy, Trust, Logging, Projects, Quota, Storage, Lifecycle, Webhooks, Notifications,
}
impl SettingsSection {
    /// `system_config` key, for example `settings.smtp`.
    #[must_use] pub const fn key(self) -> &'static str;
    /// JSON pointers of the values sealed at rest (purpose `settings.<section>.<field>`).
    #[must_use] pub const fn secret_pointers(self) -> &'static [&'static str];
    /// The application area that edits it (`nx-admin` until `ApplicationPrivileges` is open).
    #[must_use] pub const fn area(self) -> AppArea;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpTls { Implicit, Starttls }          // there is no plaintext variant, so there is no switch to forget

/// Outbound mail. The server name goes through the `Smtp` egress policy; the certificate is verified against the system roots
/// and the managed trust store, and verification cannot be turned off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: Option<String>,
    pub password: Option<Secret>,                 // sealed; the API shows `password_set`
    /// RFC 5322 mailbox address, validated with an address crate; CR, LF and NUL are refused everywhere.
    pub from_address: String,
    pub from_name: String,                        // at most 100 characters
    pub timeout_secs: u8,                         // 1 to 60, default 10
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyApplication { pub upstreams: bool, pub registries: bool, pub webhooks: bool, pub scanners: bool }

/// The only source of an outbound proxy: environment variables stay ignored (format-core). Blob store endpoints never use it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundProxySettings {
    pub enabled: bool,
    /// `http://host:port` or `https://host:port`, no user info; the host must be listed in `egress.proxy.allowed_hosts`.
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    /// Exact hosts, `*.suffix` hosts and CIDRs that bypass the proxy; at most 100 entries of at most 253 characters.
    pub no_proxy: Vec<String>,
    pub username: Option<String>,
    pub password: Option<Secret>,                 // sealed
    pub apply_to: ProxyApplication,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustSettings {
    /// Allow `POST /trusted-certificates/actions/fetch` (read a server's chain for review). Default false.
    pub allow_fetch: bool,
    /// Keep the platform and webpki roots in the client roots (default true); false makes the managed store the only trust.
    pub use_system_roots: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogOverride { pub target: String, pub level: LogLevel, pub expires_at: OffsetDateTime }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel { Error, Warn, Info, Debug, Trace }
/// Time boxed, cluster wide. At most 20 entries, expiry at most 24 hours ahead (default 1 hour in the API).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LoggingSettings { pub overrides: Vec<LogOverride> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectCreation { Everyone, AdminsOnly }          // default AdminsOnly (ADR-W2-3)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMappingPolicy {
    /// Default false: only `nx-admin` adds group rows to a project.
    pub project_admins_may_map: bool,
    pub oidc_path_prefixes: Vec<String>,           // must start with '/'
    pub ldap_dn_suffixes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotSettings { pub name_prefix: String, pub default_ttl_days: u32, pub max_ttl_days: u32,
                           pub allow_never_expire: bool, pub expired_retention_days: u32 }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsSettings { pub creation: ProjectCreation, pub group_mapping: GroupMappingPolicy, pub robots: RobotSettings }

/// A certificate in the managed outbound trust store (`AUTH-85`). Public data. One certificate per record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustedCertificate {
    pub id: TrustedCertificateId,
    pub name: String,
    pub pem: String,
    pub subject: String,
    pub issuer: String,
    pub fingerprint_sha256: String,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
    pub is_ca: bool,
    /// A leaf (a pinned self-signed server certificate) is accepted only when the uploader says so explicitly.
    pub allow_leaf: bool,
    pub created_by: Option<UserId>,
    pub created_at: OffsetDateTime,
}
```

The smaller sections, with their defaults (each is a `deny_unknown_fields` struct in the same file):

| Section | Fields and defaults | Used by |
|---|---|---|
| `quota` | `default_project_bytes: Option<u64>` (none), `default_warn_percent: u8` (80) | W2-C, copied into a new project's limit |
| `storage` | `last_downloaded_interval_secs: u32` (3600) | W2-C `record_downloads` |
| `lifecycle` | `dry_run_validity_days` (14), `max_delete_ratio: f32` (0.9), `mass_delete_min_candidates` (20), `batch_size` (500, 50 to 2000) | W2-C engine |
| `webhooks` | `enabled: bool` (true), the system wide switch of `OPS-05` | W2-E dispatcher |
| `notifications` | `task_recipients: Vec<String>` (empty, at most 20 addresses) | e-mail for `task.finished` |

Default values come from `Default` impls in core, so a node with no row behaves identically on every node, and a missing row is never an error.

Read only mode is `system.read_only` (section 4.d), a key of its own with its own endpoint, because it has a quiesce protocol, not a settings form. It is stored and propagated by the same framework.

**Status checks.**

```rust
// core: crates/rustryx-core/src/status.rs  (new file)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity { Info, Warning, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState { Ok, Failing, Unknown }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusCheck {
    pub id: String,
    pub severity: CheckSeverity,
    pub state: CheckState,
    /// One line, no secrets, no addresses of internal systems, at most 200 characters.
    pub message: String,
    pub since: Option<OffsetDateTime>,
    /// A blob store, scanner or registry name when the check is about one.
    pub subject: Option<String>,
}

/// Each stream registers providers; the API runs them all.
#[async_trait]
pub trait StatusCheckProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Cancel safe, read only, bounded: the runner gives it 5 seconds and reports `Unknown` on a timeout. A provider never
    /// reports `Ok` for something it could not check.
    async fn run(&self, now: OffsetDateTime) -> Vec<StatusCheck>;
}
```

Check catalog (`GET /api/v1/system/status`, results cached 30 s; `Critical` and `Failing` is what an operator is paged on):

| Check id | Owner | Failing when | Severity |
|---|---|---|---|
| `anonymous_access_enabled` | phase 1 | `auth.anonymous_enabled` is true | warning |
| `robots_never_expire` | B | a robot has no expiry | warning |
| `robots_orphaned` | B | a robot's creator is disabled or gone | warning |
| `blob_store_over_soft_quota` | C | a store is over its soft quota | warning |
| `blob_store_unhealthy` | F | a store or group member fails its probe | critical |
| `verify_blobs_missing` | F | the last verify run found missing objects | critical |
| `read_only_mode` | C | enabled (warning), enabled longer than 1 hour (critical) | warning, critical |
| `scanner_down` | D | an enabled scanner fails its probe | warning |
| `scanner_dataset_age` | D | the current data set is older than 7 days (warning) or 30 days (critical) | warning, critical |
| `osv_online_enabled` | D | `scan.osv_mode = online` (coordinates leave the site) | warning |
| `webhook_suspended` | E | a webhook is suspended | warning |
| `outbox_lag` | E | a consumer lags more than 15 minutes | critical |
| `registry_unreachable` | E | a registry failed its last probe | warning |
| `smtp_unverified` | H | SMTP is enabled and was never verified | info |
| `certificate_expiring` | H | a trusted certificate expires within 30 days (warning) or 7 days (critical) | warning, critical |
| `trust_fetch_enabled` | H | `allow_fetch` is true | info |
| `settings_skew` | H | live nodes report different settings revisions for more than 2 minutes | warning |
| `cluster_version_skew` | C | live nodes run different feature levels for more than 1 hour (info) or 24 hours (warning) | info, warning |
| `last_backup_verified` | ops | `rustryx backup verify` recorded a success more than 35 days ago, or never | warning, info |

### 8.b Ports

```rust
// core: crates/rustryx-core/src/ports/system.rs  (part 4)
#[derive(Debug, Clone)]
pub struct ConfigEntry { pub key: String, pub value: serde_json::Value, pub revision: i64, pub updated_at: OffsetDateTime, pub updated_by: Option<UserId> }

/// Whether sealed fields come back as plain values (runtime consumers) or removed (API responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretView { Mask, Reveal }

#[async_trait]
pub trait SystemConfigRepo: Send + Sync {                       // gap 19
    /// With `Mask`, a sealed field is removed from the value and its pointer is listed in the entry's value under
    /// `"_sealed": ["/password"]`, so the API can say `password_set: true` without the value.
    async fn get(&self, key: &str, view: SecretView) -> Result<Option<ConfigEntry>, StoreError>;
    async fn list_prefix(&self, prefix: &str, view: SecretView) -> Result<Vec<ConfigEntry>, StoreError>;
    /// Writes `value`, sealing the strings at the JSON pointers in `seal` (purpose `settings.<key>`, row = the key), assigns
    /// the next revision from `system_config_revision_seq`, and publishes `InvalidationEvent::Config(revision)`. With
    /// `expected_revision`, `Conflict` on a stale write. A sealed field that is absent in `value` keeps its stored value; a
    /// JSON `null` clears it (so a form can leave the password blank).
    async fn put(&self, key: &str, value: &serde_json::Value, seal: &[&str], expected_revision: Option<i64>,
                 actor: Option<UserId>) -> Result<ConfigEntry, StoreError>;
    async fn delete(&self, key: &str, actor: Option<UserId>) -> Result<(), StoreError>;
    /// Highest revision of any `settings.*` or `system.*` row: the revision a node compares with.
    async fn current_revision(&self) -> Result<i64, StoreError>;
}

#[async_trait]
pub trait TrustedCertificateRepo: Send + Sync {
    async fn create(&self, certificate: &TrustedCertificate) -> Result<(), StoreError>;      // Conflict on a duplicate fingerprint or name
    async fn get(&self, id: TrustedCertificateId) -> Result<Option<TrustedCertificate>, StoreError>;
    async fn list(&self) -> Result<Vec<TrustedCertificate>, StoreError>;
    /// `Conflict` while a proxy repository, registry or scanner lists it; `dependents` names them.
    async fn delete(&self, id: TrustedCertificateId) -> Result<(), StoreError>;
    async fn dependents(&self, id: TrustedCertificateId) -> Result<CertificateDependents, StoreError>;
}
pub struct CertificateDependents { pub repositories: Vec<RepositoryId>, pub registries: Vec<RegistryId>, pub scanners: Vec<ScannerId> }
```

Typed access for consumers is a crate level handle, so no component parses JSON:

```rust
// crate: rustryx-api (module settings; the handle type is re-exported to the other crates through a tiny trait in rustryx-egress)
pub struct SettingsSnapshot { pub revision: i64, pub smtp: SmtpSettings, pub outbound_proxy: OutboundProxySettings,
                              pub trust: TrustSettings, pub logging: LoggingSettings, pub projects: ProjectsSettings,
                              pub quota: QuotaSettings, pub storage: StorageSettings, pub lifecycle: LifecycleSettings,
                              pub webhooks: WebhooksSettings, pub notifications: NotificationSettings,
                              pub read_only: Option<ReadOnlyState> }
pub trait SettingsSource: Send + Sync {
    fn snapshot(&self) -> Arc<SettingsSnapshot>;                     // arc-swap, wait free
    fn subscribe(&self) -> tokio::sync::watch::Receiver<Arc<SettingsSnapshot>>;
}
```

### 8.c PostgreSQL

Migration `0120_trusted_certificates.sql`. The global settings revision (a sequence and a bump trigger on `system_config`) is part of the foundation migration 0007, because read only mode and the node registry need it before this stream starts.

```sql
-- 0120
CREATE TABLE trusted_certificates (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    pem text NOT NULL CHECK (length(pem) <= 16384 AND pem NOT LIKE '%PRIVATE KEY%'),
    subject text NOT NULL CHECK (length(subject) <= 512), issuer text NOT NULL CHECK (length(issuer) <= 512),
    fingerprint_sha256 text NOT NULL CHECK (fingerprint_sha256 ~ '^[0-9a-f]{64}$'),
    not_before timestamptz NOT NULL, not_after timestamptz NOT NULL,
    is_ca boolean NOT NULL, allow_leaf boolean NOT NULL DEFAULT false,
    created_by uuid, created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_trusted_cert_name UNIQUE (name), CONSTRAINT uq_trusted_cert_fp UNIQUE (fingerprint_sha256),
    CONSTRAINT ck_trusted_cert_leaf CHECK (is_ca OR allow_leaf)
);
```

Retention: none (settings rows live until deleted; the audit rows keep the history of changes). Expand and contract: the migration is expand; the revision column of `system_config` is one that N-1 binaries do not read. No gate, no contract.

The rows: `settings.smtp`, `settings.outbound_proxy`, `settings.trust`, `settings.logging`, `settings.projects`, `settings.quota`, `settings.storage`, `settings.lifecycle`, `settings.webhooks`, `settings.notifications`, `system.read_only`, `security.revision`, `quota.counters_ready`. Sealed fields are stored as `rx1.` envelopes inside the JSON with the AAD `settings.<key>:<pointer>`, so a value copied to another key or pointer does not open.

### 8.d Jobs

| Job | What it does | Notes | Metrics |
|---|---|---|---|
| `settings_refresh` (node daemon, 5 s, and at once on `Config(rev)`) | `list_prefix("settings.")` plus the `system.*` keys, parse into a new `SettingsSnapshot`, swap it, notify subscribers (egress client rebuild on SMTP, proxy or trust changes, log filter rebuild on logging changes). The node's applied revision goes into the next heartbeat | a section that fails to parse keeps the previous value for that section and raises a warning log and `settings_parse_errors_total{section}`; one bad row never blocks the others; the 5 s poll is the fallback when the bus is down, so read only mode takes effect within 5 s even then | `settings_revision_applied` (gauge), `settings_changes_total{section}` |
| `cert_expiry_check` (daily 06:30) | writes the input of `certificate_expiring`; excludes expired certificates from the client roots (the exclusion also happens at snapshot build time, so a certificate that expires at midnight stops being trusted at midnight, not at the next job) | read only | standard |

### 8.e Authorization

| Operation | Requires |
|---|---|
| read any settings section (secrets always masked) | `App(Settings, Read)`, phase 1 `nx-admin` |
| `smtp`, `outbound_proxy`, `trust`, `logging`, `notifications`, `projects` | `App(Settings, Write)`, phase 1 `nx-admin`; `logging` and `trust.allow_fetch` stay `nx-admin` even after application privileges exist |
| `quota` | `App(Quotas, Write)` |
| `storage` | `App(Blobstores, Write)` |
| `lifecycle` | `App(Tasks, Write)` |
| `webhooks` (system switch) | `App(Webhooks, Write)` |
| SMTP verify, send test mail | `App(Settings, Write)`; limited to 6 per hour |
| trusted certificates: list | `App(Settings, Read)` |
| trusted certificates: create, delete, fetch | `App(Settings, Write)`; fetch only when `trust.allow_fetch` is true |
| read only mode set and clear | `App(System, Write)`, phase 1 `nx-admin` |
| system status | `App(System, Read)`, phase 1 any authenticated principal sees `Critical` checks without detail, `nx-admin` sees all |

The settings that decide who may do what (`projects.group_mapping`, `projects.creation`) are security policy: `nx-admin` only until a dedicated area exists, because a project admin must never be able to widen the policy that limits project admins.

### 8.f Events and audit

No outbox events. Audit (`SettingsChange`): `settings.update` with `section`, the new `revision`, and the **names of the changed fields**; a secret field appears as `password: changed` and never with a value, and the previous and new non-secret values are included. `trust_certificate.create`, `.delete` (subject, issuer, fingerprint, validity, `allow_leaf`), `trust.fetch` (host, port, fingerprint of the chain seen), `logging.override` (target, level, expiry, actor), `system.read_only` (section 4.f), `smtp.test` (recipient domain only). Changes of the three sections that move a trust boundary (`outbound_proxy`, `trust`, `projects.group_mapping`) are also written to the warning log with the actor.

### 8.g Crates that change, behaviour details

**Egress client rebuild.** `rustryx-egress` owns an `EgressClientFactory` built from the config policies (section 2.5) and a `SettingsSource`. It keeps one client set per purpose behind an `ArcSwap`, rebuilt when the trusted certificates, `use_system_roots` or the outbound proxy change: the roots are the system and webpki roots (when `use_system_roots`) plus every non expired certificate of the managed store plus the repository's own `ca_certificate_ids`; the proxy applies only to the purposes ticked in `apply_to`. A rebuild happens off the request path and the old client finishes its in flight requests.

**Outbound proxy and the SSRF guard** (ADR-W2-30). Through an HTTP proxy the proxy, not Rustryx, resolves the destination and connects, so the pinning of the connected address (resolve once, connect to the checked IP) cannot be done by the node. The rules that keep the guard honest: the destination URL still passes the full policy (scheme, host and port allowlists, user info, literal address classes); the node still resolves the destination name itself and refuses when any answer is in a forbidden class, as a pre check (it cannot pin, so a rebinding that happens between that check and the proxy's own lookup is a residual risk, stated here and in the runbook, and the proxy is expected to enforce its own egress rules); the proxy address itself is exempt from the address class check but must be named in the file configuration `egress.proxy.allowed_hosts`, which an API administrator cannot edit; `no_proxy` destinations connect directly under the normal pinned path; credentials are sent only to the proxy.

**Trust store.** Upload validation: parse exactly one X.509 certificate with `x509-parser` (MIT or Apache-2.0, pure Rust), at most 16 KiB, no private key block anywhere in the PEM, `not_after` in the future, basicConstraints CA true unless `allow_leaf`, a duplicate fingerprint is a conflict, at most 200 certificates. The fetch helper (`POST /trusted-certificates/actions/fetch`) exists only when `trust.allow_fetch` is true: it opens a TLS connection through the **same SSRF policy** (purpose `Probe`, public addresses only unless `egress.probe.allow_private_cidrs`), accepts any chain **only to read it**, returns subject, issuer, validity, fingerprint and PEM for the administrator to review, stores nothing and trusts nothing, is limited to 6 calls per minute and is audited. Trusting it is a separate upload.

**Log levels.** The handle is `rustryx_observability::LogControl`, built on `tracing_subscriber::reload` over the filter that `observability.log_level` defines. The API stores overrides in `settings.logging` (cluster wide, time boxed, at most 20, each at most 24 hours; the endpoint `PUT /api/v1/system/loggers/{target}` writes an entry with a default of 1 hour and `DELETE` removes it). Every node applies the unexpired overrides on top of the base filter and drops an entry at its expiry by its own timer and at the next refresh. Allowed targets are a fixed list in code: the workspace targets `rustryx_*`, plus `hyper`, `reqwest` and `rustls`. Limits: `trace` is allowed only for non credential crates; the crates that handle credentials (`rustryx_auth`, `rustryx_rbac`, `rustryx_egress`) and `sqlx` are capped at `debug` and `info` respectively. The redactor of the log layer applies at every level, so a debug line still has secrets scrubbed, and the request span stays enabled whatever the level. An unknown target is a 400, not a silent no-op. The heartbeat carries a hash of each node's effective filter so an operator can see which nodes have which overrides.

| Crate | Change |
|---|---|
| `rustryx-core` | `settings.rs`, `status.rs`, `ports/system.rs` part 4, `AppArea`, `PrivilegeGrant::Application`, `InvalidationEvent::Config` |
| `rustryx-db` | migration 0120, `PgSystemConfig` becomes `SystemConfigRepo` (sealing by JSON pointer), `TrustedCertificateRepo` with the dependents query across `repositories.proxy_config`, `registries`, `scanners` |
| `rustryx-api` | settings module (`/api/v1/settings/{section}` GET and PUT with `If-Match`, `/api/v1/settings/schema` generated with `schemars` from the typed structs, SMTP verify), `/api/v1/trusted-certificates`, `/api/v1/system/{status,info,nodes,loggers,read-only}`, the `settings_refresh` daemon, the status runner, the write middleware of read only mode |
| `rustryx-egress` | client factory, proxy support, trust store roots, `Purpose::Probe` |
| `rustryx-notify` | the SMTP sender reads `SmtpSettings` from the snapshot |
| `rustryx-observability` | `LogControl`, the new metric names |
| `rustryx-config` | `egress.proxy.allowed_hosts`, `egress.probe.*` |
| `rustryx-testkit` | `MemSystemConfig` with the same sealing contract, a fake SMTP server for the sender tests |

REST resources: `GET/PUT /api/v1/settings/{section}`, `GET /api/v1/settings/schema`, `POST /api/v1/settings/smtp/verify`, `GET/POST /api/v1/trusted-certificates`, `DELETE /api/v1/trusted-certificates/{id}`, `POST /api/v1/trusted-certificates/actions/fetch`, `GET /api/v1/system/status`, `GET /api/v1/system/nodes`, `GET /api/v1/system/info` (redacted: version, feature level, node id, build info; never an environment dump), `GET/PUT/DELETE /api/v1/system/loggers/{target}`, `PUT /api/v1/system/read-only`, `GET /api/v1/system/read-only/quiesced`.

### 8.h Negative requirements

| Requirement | Test |
|---|---|
| The SMTP password, proxy password and any sealed field never appear in GET responses, logs, audit rows or error text; a PUT without the field keeps it, `null` clears it | `api::must_not_return_the_smtp_password`, `..._the_proxy_password`, `db::must_keep_a_sealed_field_when_omitted`, `observability::must_not_log_a_settings_secret` |
| A sealed value copied to another key or pointer does not open | `db::must_not_open_a_sealed_setting_copied_elsewhere` |
| No plaintext SMTP and no way to disable certificate verification | `core::must_not_represent_plaintext_smtp`, `notify::must_not_skip_certificate_verification`, `notify::must_not_send_to_a_plaintext_server` |
| SMTP host and port pass the `Smtp` egress policy; a metadata or loopback address is refused | `notify::must_not_connect_to_a_forbidden_smtp_address` |
| Mail headers cannot be injected through a from name, address or recipient | `notify::must_not_allow_header_injection_in_settings` |
| The outbound proxy host must be in the file configuration; an environment proxy is ignored; user info in a proxy URL is refused | `egress::must_not_accept_a_proxy_outside_the_allowlist`, `..._use_an_environment_proxy`, `api::must_not_accept_userinfo_in_a_proxy_url` |
| Through a proxy the destination still passes the address class pre check and the allowlists | `egress::must_not_reach_a_forbidden_destination_through_a_proxy` |
| Trust store: a private key is refused, an expired certificate is not trusted, a leaf needs the explicit flag, a certificate in use cannot be deleted | `api::must_not_accept_a_private_key`, `egress::must_not_trust_an_expired_certificate`, `api::must_not_accept_a_leaf_without_the_flag`, `db::must_not_delete_a_certificate_in_use` |
| The fetch helper is off by default, SSRF checked, rate limited, stores and trusts nothing | `api::must_not_fetch_when_disabled`, `egress::must_not_fetch_from_a_forbidden_address`, `api::must_not_trust_a_fetched_chain_automatically` |
| Log overrides: unknown target refused, `trace` refused for credential crates, expiry at most 24 hours, at most 20 entries, expiry reverts the level | `observability::must_not_allow_an_unknown_log_target`, `..._trace_on_the_auth_crates`, `..._exceed_the_override_ttl`, `must_revert_a_level_at_expiry` |
| A stale settings write is a conflict; an unknown field is refused; a section that fails to parse keeps the old value | `api::must_not_overwrite_a_newer_revision`, `core::must_not_accept_unknown_settings_fields`, `api::must_not_apply_an_unparsable_section` |
| Nodes converge: every live node reports the revision within the refresh interval, even with the bus down | `api::nodes_converge_on_a_settings_revision_without_the_bus` |
| A status check that times out or errors is `Unknown`, never `Ok`; messages contain no secrets or internal addresses | `api::must_not_report_ok_for_an_unchecked_item`, `..._leak_an_address_in_a_check_message` |
| A project admin cannot edit the settings that limit project admins | `api::must_not_let_a_project_admin_edit_group_mapping_policy` |

---

## 9. Negative requirement index

Each stream lists its own `must_not_*` tests in its `h` part. This table maps the cross-cutting classes of the brief to the tests that carry them, and names the shared harness so a class is tested the same way everywhere. A class is accepted only when every row exists and runs in CI.

| Class | Rule | Carried by | Shared harness (in `rustryx-testkit`) |
|---|---|---|---|
| SSRF in replication, webhook, registry, scanner, SMTP and trust fetch targets | every outbound target passes the per purpose egress policy on create and on every call; one DNS lookup, the connected address is the checked one; no redirect for webhooks; internal CIDRs only through file configuration | 6.h rows 1 to 3, 5.h scanner endpoint rows, 8.h SMTP and fetch rows, `egress::must_not_*` | `ssrf_matrix`: the class list of format-core section 10 (loopback, private, link local, metadata, CGNAT, multicast, unspecified, mapped, NAT64, 6to4, alternate notations, user info, schemes, rebinding, redirect) run once per `Purpose` with a hostile fake resolver |
| Secrets never in responses or logs | webhook secret and auth header, registry password, SMTP and proxy passwords, scanner credentials, robot and scan tokens, presigned URLs, blob store credentials | 3.h, 5.h, 6.h, 7.h, 8.h secret rows; the type level test of gap 34 | `canary`: creates each entity with a canary value, drives every route, then searches every response body, `Debug` output, log line, audit row and metric label of the run for the canary |
| HMAC verification is constant time and replay safe | signature over timestamp and body, `subtle` or `hmac::verify`, 300 s window | 6.h signature row | a source scan test that fails on `==` between signature types, plus `sign`/`verify` vectors |
| Pull gate fails closed | unknown verdict, failed scan, store down in `Enforce` mean deny (503 for infrastructure), never allow; scanner token exempt, nothing else | 5.h gate rows | `gate_matrix`: `evaluate` over the product of (state, policy, mode, allowlist) with a store that can be made to fail |
| Immutability beats delete and overwrite | by SQL guard, so also for cascades, N-1 writers, lifecycle, promotion and replication; override only by an admin run | 4.h immutability rows | `guard_matrix`: every state changing port method against an immutable and a held item, on `MemStore` and on PostgreSQL |
| Quota before and after the write | pre check on the declared length, authoritative conditional update on the actual size in the publish transaction, a refusal leaves nothing | 4.h quota rows | `quota_race`: concurrent publishers against a limit, content length that lies, shared blobs |
| Cross-project isolation | a project role, a robot scope, a label, a webhook, a promotion, a CVE allowlist or a setting of one project never reaches another project or a repository without one | 3.h first rows, 5.h allowlist row, 6.h last row, 8.h group mapping row | `tenant_matrix`: two projects, a user in each, every project scoped route and every content route |
| Cluster safety (N-1) | a gated feature is refused while an older node is live; DB guards apply to every binary | each stream's last row | `two_version_cluster`: a fake node registry with one node below the level |
| Route declaration | every route declares `Requires`; 401 without credentials, 403 without the privilege | the api crate's route enumeration (SEC-11) | `route_matrix` over the router, reading the `Requires` values |

All ports get one **conformance suite** that runs against `MemStore` and `PgStore` with the same assertions (missing row semantics of gap 3, guards, keyset paging, optimistic version conflicts, unique constraints, the outbox hand-off). The in memory doubles are allowed to be looser than PostgreSQL only where testkit.md says so; the guards are not among those places.

---

## 10. Spikes: what each must prove

A spike has a fixed question, fixtures that exist before the code, pass criteria that are numbers or yes/no facts, and a fallback chosen in advance, so the result is a decision and not an opinion. Every spike ends with an ADR update. Items here are `unverified` today.

| Id | Question | Owner | Blocks |
|---|---|---|---|
| S1 | How is Trivy driven safely and offline? | D | D1 |
| S2 | Wrap `osv-scanner` or write a native OSV matcher? | D | D2 |
| S3 | How is Cosign verified in Rust? | D | D3 |
| S4 | How is Notation verified? | D | D3 (Notation part) |
| S5 | Does `object_store` give a correct Azure store? | F | F2 |
| S6 | Does `object_store` cover the S3 options, presign and SSE? | F | F2 |
| S7 | Does the outbox relay lose or duplicate nothing under concurrency? | C | E1 |
| S8 | Is the glob to regex translation equal to a reference matcher and bounded in PostgreSQL? | C | C1 |
| S9 | Does `verify_blobs` scale to tens of millions of keys? | F | F3 |

**S1 Trivy.** Pin one Trivy release. Prove: (1) a scan of an OCI layout directory (or an image archive; whichever the pinned version accepts as local input, flag names `unverified`) exported by the product from blob store reads, with a pre seeded database and **no network**: run inside a network namespace with no route, exit status distinguishes "findings" from "tool failure", and a capture shows zero DNS queries, with every update, telemetry and remote bundle feature switched off by flags that the test lists; (2) the JSON output maps onto `Finding` for a corpus of 20 images (severity names, `Unknown`, missing CVSS, duplicate package entries, OS and language packages), and the output size cap holds on a pathological image; (3) peak RSS and wall time for images of 100 MB, 1 GiB and 3 GiB, which sets the formula for `scan.max_concurrent` and the pod memory in the chart; (4) SBOM output in CycloneDX and SPDX from the same input; (5) an index is scanned per platform manifest and the results map to the children; (6) the work directory and Trivy's cache directory are fully removed on success, error, timeout and cancel, and a kill on timeout leaves no child process (if Trivy spawns children, the runner must kill a process group). Fallback: the scan worker image (a small adapter container in front of Trivy speaking the HTTP contract of section 5.g), which costs a network hop and a second image.

**S2 OSV.** Pin one `osv-scanner` release and one OSV data snapshot. Prove: (1) it runs fully offline from a mirrored data set (flags and data layout `unverified`), taking an SBOM or purl list built by Rustryx as input; (2) the results of 500 package versions across maven, npm, pypi, cargo and pub (pub and go only if the pinned snapshot has them) are identical to the online OSV API for the same snapshot; (3) a reproducible mapping of its output to `Finding`, including aliases and fixed versions; (4) cost per batch, so `scan_rescan_stale` can run over a million components in the nightly window by batching. Decision rule: if (1) and (2) hold, ship the wrapper. A native Rust matcher is considered only if it reproduces the wrapper on a 500 package golden corpus **with zero false negatives** per ecosystem comparator (SemVer, Maven, PEP 440, Cargo); otherwise it is rejected, because version comparison is exactly where a security tool silently misses a finding.

**S3 Cosign verification in Rust.** Candidate: the `sigstore` crate (license and maintenance to be confirmed). Fixtures, generated before the spike by pinned releases of the cosign CLI (the newest stable major and the previous one), stored in the repository: (a) key based signatures (ECDSA P-256 and Ed25519) in the legacy tag scheme and in the OCI referrer form with the newer bundle format; (b) keyless signatures with a Fulcio certificate and a transparency log entry in a bundle, against a test trust root the fixtures carry; (c) attestations are out of scope. At least 40 negative fixtures: wrong key, altered payload, a payload whose `docker-manifest-digest` is another digest, a payload for another repository when the policy pins the reference, certificate expired outside the log's integrated time, missing or invalid log entry or inclusion proof when `require_transparency_log` is set, tampered certificate chain, wrong identity or issuer, a downgraded or unsupported algorithm, several signatures of which one is valid and one is not, truncated and oversized bundles. Pass criteria: all positives verify and return the signer identity; all negatives are rejected with a distinct typed reason; verification needs **no network**; p95 verification time under 25 ms per signature on one vCPU excluding the fetch; a 15 minute fuzz run of the bundle and payload parsers without a panic; license in the project allowlist, a release within the last 12 months, no open RustSec advisory; the dependency tree has no `openssl-sys` and does not build a default rustls configuration (this workspace carries both `ring` and `aws-lc-rs`, so a crate that calls `ClientConfig::builder()` panics, as core-gaps.md records). The Rustryx side of the policy is tested separately and must hold with either provider: subject digest binding, identity matching, "both schemes", children of an index. Fallback: a sandboxed `cosign` binary delivered like Trivy, fed local files, with the same policy code around it; the spike also measures that path (spawn cost, concurrency cap, exit codes, whether offline verification of local files exists in the pinned version, `unverified`).

**S4 Notation verification.** The first question is whether a maintained Rust implementation exists at all; none is known to this document (`unverified`). If one is found it must cover: JWS and COSE signature envelopes, trust store types `ca`, `signingAuthority` and `tsa`, trusted identities by certificate subject, the verification levels `strict`, `permissive` and `audit`, certificate chain validation, revocation checking (CRL and OCSP) or an explicit offline decision, and timestamp countersignatures, with the same negative fixture families as S3 (wrong store, identity mismatch, wrong subject digest, tampered envelope, expired certificate, unsupported algorithm). If none exists the spike evaluates the helper path: the pinned official `notation` CLI verifying signatures from files or an OCI layout that the product stages (flag availability `unverified`), with the trust policy and trust store directory generated by the product from `TrustPolicy` and `TrustMaterial` using UUID names only, a cleared environment, parseable exit codes and output, concurrency cap, time per verification, and a recorded decision about revocation (cached CRL offline, or disabled with the level and a status check naming it). `Skip` is never offered as a level. Expected outcome, stated so it is a hypothesis and not a fact: Cosign native if S3 passes, Notation through the helper.

**S5 Azure.** With Azurite in testcontainers and the `azure` feature of `object_store`: a staged multi block upload with 3 blocks commits atomically and is invisible before the commit; an abandoned upload leaves nothing visible and the uncommitted blocks are the only residue; ranged reads and `HEAD`; a 5 GiB upload keeps memory at one part; `Drop` and task cancellation clean up; error mapping to `BlobError`; listing pages for `list_keys`; and SAS presign for a GET through the crate's signer or a documented manual construction. The authentication modes `managed_identity` and `workload_identity` cannot run against Azurite; the spike records what was and was not tested. Fallback: the Azure SDK for Rust (if licence and maturity pass) or `azure` marked `later`.

**S6 S3 options.** Against SeaweedFS and Ceph RGW (and, where an account exists, AWS): static keys, session token, web identity, container credentials and instance metadata with IMDSv2 (the ambient chain), SSE-S3, SSE-KMS with a key id, path style and virtual host style addressing, presigned GET with `response-content-type` and `response-content-disposition`, and whether `role_arn` based `AssumeRole` is available. Pass: every row is either supported by `object_store` as pinned or has a stated route (a custom `CredentialProvider` from `aws-config`, already an accepted alternative of 02-rust-engineering). Output: the final shape of the S3 config table of section 7.a.

**S7 Outbox relay.** A property test with concurrent writers that commit, roll back and hold open for random durations against a relay with the xid8 horizon rule: no committed event is lost, no event is read twice past its cursor, order follows `(tx_id, seq)`, and a transaction held open for 60 s delays but never skips events. Measures lag at 1000 events per second. Fallback if the horizon rule misbehaves on the pinned PostgreSQL: advisory lock around outbox inserts (serializes writers, rejected unless the rule fails) or logical decoding (rejected for operational weight).

**S8 Glob translation.** A differential property test of the Rust glob to anchored regex translation against a reference glob matcher for a million random patterns and inputs, and an adversarial run proving that the translated expressions evaluate in PostgreSQL in under 5 ms for the maximum pattern (256 characters, 8 wildcards, 8 alternatives) against a 2048 character input.

**S9 `verify_blobs` scale.** Stage 50 million listed keys into the unlogged table and run both anti joins and the size comparison: time within 2 hours, memory bounded by the batch size, WAL not generated, and the plan uses the primary keys and `uq_blobs_store_key`.

**Unverified in this document, collected.** Trivy local input flags and offline flags (S1); `osv-scanner` offline flags and data layout (S2); coverage and maintenance of the `sigstore` crate and any Notation implementation (S3, S4); `notation` and `cosign` offline local file verification (S3, S4); `object_store` Azure multipart, presign and AssumeRole (S5, S6); the third party license of every delivered binary (Trivy, osv-scanner, cosign, notation; all believed to be permissive, to be confirmed against the allowlist of README 8.6 before they are delivered); the exact compat and Harbor behaviours that parity.md lists as `unverified` (none of the decisions depends on them: each is decided here on its own merits).

---

## 11. ADR table

Every open design question of parity sections 11 and 12 has a row. `Parity` names the section, `W2-A` marks questions whose implementation belongs to the API stream outside this document but whose answer is fixed here.

| ADR | Parity | Decision | Alternative | Reason |
|---|---|---|---|---|
| ADR-W2-1 | 11.1 | Deleting a project is refused while a repository is attached; there is no cascade | cascade after a typed confirmation | a delete through a policy container would skip the per repository audit and the immutability checks |
| ADR-W2-2 | 11.2 | A robot is a row of its own table `robot_tokens` with the `ryx_` format, exposed as one API resource with personal tokens | rows in `user_tokens` with a kind column | an old pod ignores an unknown table cleanly, deleting a creator must not delete robots, and the owner column would have to become nullable |
| ADR-W2-3 | 11.1, OPS-24 | Project creation is `AdminsOnly` by default | everyone | a tenant boundary should be a conscious act; flipping it is one setting |
| ADR-W2-4 | 11.1 | Group rows of a project are added by `nx-admin` unless the policy lists allowed path prefixes or DN suffixes | free mapping by project admins | a group row gives access to people the project admin does not manage |
| ADR-W2-5 | 11.1 | `allow_anonymous` of a project repository follows the project visibility by trigger | derive it at read time by a join | cascades, N-1 writers and forgotten code paths cannot make a repository public against its project |
| ADR-W2-6 | 11.2 | Robot expiry is mandatory (30 days default, 365 maximum); never-expire needs `nx-admin` and a setting; the Basic user name is ignored | name must match the robot | `twine` style clients send fixed names; mandatory expiry keeps a leaked robot finite |
| ADR-W2-7 | 11.9 | One metadata mechanism, labels with a scope; the compat tag API is a compatibility view; "tag" means OCI tag | two mechanisms | one table, one UI, no clash of two meanings of tag |
| ADR-W2-8 | 11.3, 12.2, 16 | One lifecycle engine, two modes; a real run needs a dry run of the same policy version; a mass delete brake; `cleanup` daily at 01:00 UTC | run without a dry run; hourly | a deletion needs a reviewed plan; the two compat pages disagree on the default, and a heavy statement set needs no more than daily |
| ADR-W2-9 | 11.3 | No tombstone column; a lifecycle delete removes rows and `gc.min_age` is the grace | `deleted_at` on every table | every query and index would need the filter; the trash view is `later` (`SRCH-20`) |
| ADR-W2-10 | 11.4 | Immutability selectors are globs evaluated in SQL by triggers; an identical re-push is allowed; delete is blocked (remove the rule first, which is audited) | CEL evaluated by the application | the check must run in the publish transaction where no CEL engine exists, and a rule saved between evaluation and commit would race |
| ADR-W2-11 | 11.5 | Quota by incremental counters and one conditional update in the publish transaction; the repository limit stays in `repositories.quota_bytes`; counters are authoritative after the first recount | `SUM` on every upload, or a lock per project | the sum is proportional to the number of assets per upload; the conditional update is atomic and holds the row lock only at the end |
| ADR-W2-12 | 11.11, 11.5 | Proxy cache content counts in the quota; a 7 day retention template is offered; an optional namespace allowlist and a bandwidth limit exist | exclude the cache from quota | a proxy would otherwise fill a disk unmonitored; the reference product's behaviour here is `unverified` |
| ADR-W2-13 | 11.12 | GC marks, then sweeps per blob with the row deleted first under a row lock and the object second; a dry run stores a report; history 90 days | object first (the phase 1 wording) | with `link` an orphan can be re-referenced, and deleting the object first could leave an asset pointing at nothing |
| ADR-W2-14 | 12.3 | Schedules fire by compare and set of `next_run_at`, runs execute under a distributed lock for singleton kinds, cron is UTC | a lock for the scheduler too, local time zones | no connection per tick, exactly once by construction, no DST |
| ADR-W2-15 | OPS-22 | Read only mode is a flag enforced by middleware, runner and SQL guard, answered with 503 `read_only`, with a quiesce endpoint | middleware only | a missed code path must not corrupt a backup |
| ADR-W2-16 | 11.7 | Trivy runs as a subprocess on an exported local input from a per scan work directory | Trivy client and server mode, or a scan through registry pull | no network, no credentials, no interplay with the gate and the counters, and DSO-22 keeps the binary out of the image |
| ADR-W2-17 | 11.7 | Wrap the official `osv-scanner` offline first; the online mode is off by default | a native matcher now; online by default | version comparison per ecosystem is where false negatives hide; online sends coordinates to a third party |
| ADR-W2-18 | 11.7, SCN-27 | SBOM default CycloneDX JSON, SPDX JSON selectable | SPDX first | DSO-08, DSO-27 and SEC-45 already standardize CycloneDX and Trivy emits both (open question 2) |
| ADR-W2-19 | 11.7, SCN-14 | Gate default `audit_only`; `enforce` explicit and gated; unscanned is denied in `enforce` unless allowed; a verdict error is 503; scanner tokens are exempt, replication is not | fail open on error | the gate is a security control (DSO-23, DSO-17) |
| ADR-W2-20 | 11.10 | Policy logic is Rustryx code; the signature primitive comes from spikes S3 and S4; presence alone is never accepted; `verify_children` defaults true | trust that a signature exists | presence proves nothing; an index must not be a bypass |
| ADR-W2-21 | 11.8 | Outbox with an xid8 horizon relay, at least once, signature over timestamp and body in the DSO-32 header, no redirects | dispatch after commit; body only HMAC | an event must exist if and only if the change committed; a body only signature has no replay protection (SEC-38 is stricter) |
| ADR-W2-22 | 11.6 | Per artifact tasks on a lease queue, one active execution per rule by a unique index, referrers after the subject, a protocol range for Rustryx peers | a lock per rule | no lock connection, resumable, no fan out of locks |
| ADR-W2-23 | DSO-28 | Promotion is request, counted approvals, checks evaluated again at execution, a hold during the run | direct copy into release repositories | TOCTOU between approval and run, and DSO-28 requires an approver who is not the author |
| ADR-W2-24 | 12.4 | Security boundaries and bootstrap values in file or environment configuration, operator policy in database settings sections | everything in the database, or everything in the file | an API administrator must not be able to widen the SSRF allowlists |
| ADR-W2-25 | 14 | Extract `rustryx-egress` from format-core | scan, replication and notify depend on format-core | dependency direction |
| ADR-W2-26 | N-1 | Feature levels in a node heartbeat table gate new data kinds | an operator flag, or no gate | automatic, cannot be forgotten, and visible in `GET /system/nodes` |
| ADR-W2-27 | 12.9 | A blob row names its member store; round robin is `hash(key) mod n`; convert to a group by repointing; remove a member by a move task with a delayed cleanup | probe members on read; a shared round robin counter | reads never probe, no coordination between nodes, no data moves on conversion |
| ADR-W2-28 | BLOB-16 | Presigned redirects off by default, per store, never for pub or manifests, TTL at most 900 s, URL treated as a secret | always redirect | the Authorization prefix rule of pub, and a URL is a bearer capability |
| ADR-W2-29 | BLOB-04 to 07 | Azure and S3 through `object_store`; `aws-config` only if AssumeRole is required | the vendor SDKs | one trait, fewer dependencies, the multipart design already exists |
| ADR-W2-30 | PRX-07 | The outbound proxy host comes from file configuration; the destination keeps its pre check; the residual rebinding risk is documented | environment proxies, or no proxy support | the address cannot be pinned through a proxy |
| ADR-W2-31 | 12.5 | Routing rules use the linear time `regex` crate, are checked before the cache lookup (a new block rule applies to cached content at once), deleting an assigned rule is 409, there is a test endpoint | a backtracking engine | ReDoS, and the reference behaviour for cached content is `unverified` |
| ADR-W2-32 | 12.1 | Repository health is an aggregate over stored findings plus the `health_check` flag; license categories are `later` | a separate subsystem with vendor data | no vendor call, one data path |
| ADR-W2-33 | 12.4 | No capability framework; typed, validated, audited sections | a generic capabilities list | typed validation, one schema per section |
| ADR-W2-34 | 12.6 (W2-A) | `POST /api/v1/components` calls the same publish use case as the native protocol, so write policy, immutability, quota, audit and events apply exactly once | a separate upload path | two paths would diverge |
| ADR-W2-35 | 12.7 (W2-A) | The browse tree is maintained inside the publish and delete transactions; proxy repositories show cached content only; a group shows the merge with first wins; OCI uses the component list | a rebuilt index; cached plus remote | no rebuild task, and a browse must not trigger an upstream call |
| ADR-W2-36 | 12.8 (W2-A) | `Application` privileges for administration areas (reserved in Core revision 2), selector preview capped at 10 results, folders visible when a permitted asset lies below, CSEL translation `later` | a free form wildcard privilege | additive only model stays intact |
| ADR-W2-37 | 11.7 | A scan access token is its own `AuthMethod`, bound to one repository, 15 minutes | a robot token per scan | the exemption from the gate and counters must be explicit in code |
| ADR-W2-38 | BLOB-19 | `verify_blobs` stages the listing in an unlogged table and anti joins; repair deletes orphans only | an in memory key set; automatic row deletion | scale, and a missing object is evidence of data loss, not tidiness |
| ADR-W2-39 | 11.1 | The five project roles are a code table (`ProjectRole::template`), not database rows | custom project roles | a fixed matrix is auditable and pinned by a test; custom roles are the existing role system |
| ADR-W2-40 | 11.8 | The event catalog is a typed enum in core with dotted wire names | free strings | subscriptions are validated and a producer cannot invent a kind |

---

## 12. Implementation order and sizes

Size classes follow the parity document (S a few days, M a few weeks, L more than a month, for one engineer); the weeks are rough engineer weeks with a spread of about plus or minus 40 percent. They include the stream's own tests, docs and the spikes it owns.

### 12.1 Phases

| Phase | Starts when | In parallel |
|---|---|---|
| 0 | now | Core revision 2 and the foundation migration, adaptation of the crates (section 1.6), stub crates, spikes S7 and S8 prepared |
| 1 | phase 0 merged | **C0** task framework, outbox, node registry and feature gate; **X0** `rustryx-egress` extraction; **B1** projects, members, rbac project rules; **F1** group store and registry trait; **H1** settings framework and trust store; spikes S1, S5, S6 |
| 2 | C0 merged (X0 for E and the D HTTP adapter) | **B2** robots and labels; **C1** downloads, quotas, immutability, holds, read only; **D1** scanner trait, Trivy, queue, reports; **E1** registries, webhooks, outbox consumers; **F2** Azure, S3 options, presign; **H2** SMTP and outbound proxy; spikes S2, S3, S4 |
| 3 | the named dependencies | **C2** lifecycle engine and GC history; **D2** OSV, allowlists, gate, quarantine, hub, SBOM, data sets; **E2** replication and e-mail; **F3** verify and member move; **H3** log overrides and status checks |
| 4 | D2, C1, E1 | **D3** trust policies; **E3** promotion and copy |

### 12.2 Dependencies

| Step | Needs |
|---|---|
| C0 | Core revision 2 |
| B1 | Core revision 2 (the foundation tables) |
| X0 | Core revision 2 |
| F1 | Core revision 2 |
| H1 | Core revision 2 |
| B2 | B1 |
| C1 | C0; B1 for the project scope (the repository scope can start before it) |
| C2 | C1, B1, the OCI contract of section 4.g for the OCI parts |
| D1 | C0; the OCI contract; S1 |
| D2 | D1, B1, X0, S2 |
| D3 | D2, S3 and S4 |
| E1 | C0, X0, H1 (egress rebuild on settings) |
| E2 | E1, the OCI contract, B2 (label filter), H2 (SMTP) |
| E3 | E1, D2 (checks), C1 (immutability, holds), F1 (same store linking) |
| F2 | F1, S5, S6 |
| F3 | F1, C0 |
| H2 | H1, X0 |
| H3 | H1, plus the providers of the streams as they land |

Outside this document and shared by several of the above: the phase 1 OCI crate (iteration 3), which owns tags, referrers, blob mounting and the catalog and tag ports of section 4.g; W2-A, which owns the native `/api/v1` router, the OpenAPI document, the browse tree and the audit viewer, and which every stream's REST resources list feeds.

### 12.3 Rough size per stream

| Stream | Class | Engineer weeks | Parts |
|---|---|---|---|
| Core revision 2 and adaptation | M | 3 (lead plus crate owners) | 1 for core, 2 for the adaptations |
| X0 egress extraction | S | 1 | |
| W2-B projects, roles, robots, labels | L | 8 | B1 5, B2 3 |
| W2-C lifecycle and capacity | L | 14 | C0 3, C1 5, C2 6 |
| W2-D supply chain | L | 17 | D1 5, D2 8, D3 4 |
| W2-E distribution | L | 14 | E1 4, E2 6, E3 4 |
| W2-F storage | M | 8 | F1 3, F2 3, F3 2 |
| W2-H platform settings | M | 8 | H1 3, H2 2, H3 3 |
| Total | | about 73 | |

With five engineers on the parallel tracks (C, B, D, E and a shared F and H engineer) the critical path is C0, then C1 or D1, then D2, then D3 and E3: roughly 4 to 5 months wall clock if the spikes land in phase 2 as planned. The two risks that move it are S3 and S4 (a helper process for content trust adds about 2 weeks to D3) and the OCI crate arriving late (it blocks the OCI parts of C2, D and E).

---

## 13. Open questions for the product owner

| # | Question | Why it is asked | Default used here |
|---|---|---|---|
| 1 | Who may create projects, and may project admins map identity provider groups? | tenant boundary and who can widen access | admins only; project admins may not map groups |
| 2 | SBOM default format: CycloneDX (as DSO-08 and DSO-27) or SPDX (as the reference product)? | one flag, but the UI and exports follow it | CycloneDX, SPDX selectable |
| 3 | May a new project start with the pull gate in `enforce` mode, and are unscanned images denied in `enforce`? | a registry full of unscanned images stops working on day one | `audit_only` everywhere, unscanned denied only in `enforce` |
| 4 | Is auto promotion (promote when the checks pass, no human) in scope? | 01-architecture lists it, DSO-28 requires an approver | not in scope; approval required for release targets |
| 5 | Is Notation verification required in the first cut, or is Cosign enough? | decides whether D3 waits for a helper binary | Cosign first, Notation after S4 |
| 6 | Should a legal hold block interactive deletes and overwrites, not only lifecycle jobs? | a hold that does not stop a delete means little, but it surprises users | yes for `legal`, no for `release` and `promotion` |
| 7 | Is a mail relay that does not offer TLS acceptable? | the design has no plaintext SMTP | not acceptable; STARTTLS or implicit TLS is required |
| 8 | Are pull events part of the default webhook catalog? | volume; they are written only while someone subscribes | opt in per webhook |
| 9 | Should repository admins (not only `nx-admin`) manage retention and immutability for repositories that have no project? | until application privileges exist those are `nx-admin` only | `nx-admin` only |
| 10 | Quota semantics: referenced distinct blobs per scope, and garbage collection does not reduce usage (the reference product differs). Acceptable? | surprising for people who know the other product | yes |
| 11 | May replication copy from a source that the pull gate would block? | the design refuses | refused |
| 12 | Is it acceptable to deliver Trivy, osv-scanner, cosign and notation as separate binaries from an init container (licences to be confirmed against the README 8.6 allowlist)? | DSO-22 for Trivy, the same model extended to three more tools | yes, after the licence check |
| 13 | Retention of lifecycle run records (180 days) and promotion records (365 days): do compliance rules ask for more? | they are the deletion record of DSO-34 | 180 and 365 days |

Confirmed by the product owner on 2026-10-24: all thirteen defaults in this table are accepted.
