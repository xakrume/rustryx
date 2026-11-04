//! Mapping of `sqlx` errors onto [`StoreError`].
//!
//! Rules that keep errors safe to log and to show:
//! - `Conflict`, `NotFound` and `Unavailable` carry fixed, static texts. A `PostgreSQL`
//!   unique-violation message embeds the offending key value
//!   (`Key (username)=(alice) already exists`), and any error text may contain a
//!   fragment of the statement, so none of it is copied into those variants.
//! - The `source` of a `Backend` error is a [`DbFault`]: SQLSTATE plus the server's
//!   primary message only. The detail and hint fields (which carry row values) and the
//!   statement are dropped, so even a `Debug` dump of the chain holds no row data.
//! - The one exception to the last rule is the DETAIL of the policy guards `RX004` to `RX006`:
//!   the guard puts an identifier there (the immutability rule, the hold, the quota scope), and
//!   [`map_mutation_err`] parses it into a typed [`PolicyViolation`]. It is parsed, never
//!   copied: a detail that does not have the documented shape is dropped, and the error then
//!   degrades to a `Conflict` with a fixed text.

use std::fmt;

use rustryx_core::StoreError;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::ids::{ImmutabilityRuleId, LifecycleHoldId, ProjectId, RepositoryId};
use rustryx_core::quota::{QuotaExceeded, QuotaScope};
use sqlx::error::DatabaseError;
use sqlx::postgres::PgDatabaseError;
use uuid::Uuid;

/// SQLSTATE raised by the role nesting guard.
pub const SQLSTATE_ROLE_CYCLE: &str = "RX001";
/// SQLSTATE raised when a built-in role or privilege is modified.
pub const SQLSTATE_BUILTIN_READONLY: &str = "RX002";
/// SQLSTATE raised when the append-only audit table is modified.
pub const SQLSTATE_AUDIT_APPEND_ONLY: &str = "RX003";
/// SQLSTATE raised by the immutability guard (DETAIL: the rule id).
pub const SQLSTATE_IMMUTABLE: &str = "RX004";
/// SQLSTATE raised by the hold guard (DETAIL: the hold id).
pub const SQLSTATE_HELD: &str = "RX005";
/// SQLSTATE raised when a quota would be exceeded (DETAIL:
/// `<project|repository>:<scope id>:<limit>:<used>:<requested>`).
pub const SQLSTATE_QUOTA_EXCEEDED: &str = "RX006";
/// SQLSTATE raised by the read only mode guard.
pub const SQLSTATE_READ_ONLY: &str = "RX007";
/// SQLSTATE raised by the scope guards (a label on a component of another project, the last
/// project administrator).
pub const SQLSTATE_SCOPE_VIOLATION: &str = "RX008";
/// SQLSTATE raised when a component already carries 50 labels.
pub const SQLSTATE_LABEL_LIMIT: &str = "RX009";

/// Sanitised error source for logs: no statement text, no row values.
#[derive(Debug)]
pub struct DbFault {
    /// Five character SQLSTATE, when the server reported one.
    pub sqlstate: Option<String>,
    /// Primary server message or a fixed client side description.
    pub message: String,
}

impl fmt::Display for DbFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.sqlstate {
            Some(code) => write!(f, "database error {code}: {}", self.message),
            None => write!(f, "database error: {}", self.message),
        }
    }
}

impl std::error::Error for DbFault {}

impl DbFault {
    /// A client side fault with a fixed description.
    pub(crate) fn new(message: &str) -> Self {
        Self {
            sqlstate: None,
            message: message.to_owned(),
        }
    }
}

/// Static texts for constraint violations, keyed by constraint (or index) name.
const CONSTRAINT_MESSAGES: &[(&str, &str)] = &[
    ("uq_blob_stores_name", "blob store name already exists"),
    ("uq_repositories_name", "repository name already exists"),
    ("uq_blobs_store_key", "blob key already exists in the store"),
    ("uq_components_coordinate", "component already exists"),
    ("uq_assets_repo_path", "asset path already exists"),
    ("uq_users_username_ci", "username already exists"),
    ("uq_users_external", "external identity already exists"),
    ("uq_roles_name", "role name already exists"),
    ("uq_privileges_name", "privilege name already exists"),
    ("uq_selectors_name", "content selector name already exists"),
    ("uq_user_tokens_hash", "token already exists"),
    ("uq_idp_mapping_group", "group mapping already exists"),
    ("uq_job_leases_dedupe", "job is already queued or running"),
    ("uq_projects_name", "project name already exists"),
    ("uq_routing_rules_name", "routing rule name already exists"),
    ("uq_cleanup_policies_name", "cleanup policy already exists"),
    (
        "ck_immutability_max_per_scope",
        "a scope has at most 15 immutability rules",
    ),
    (
        "ck_immutability_scope",
        "an immutability rule belongs to a project or a repository",
    ),
    (
        "ck_routing_rules_patterns",
        "a routing rule needs 1 to 32 patterns of 1 to 256 bytes",
    ),
    ("uq_task_definitions_name", "task name already exists"),
    (
        "uq_task_runs_active",
        "a run of this task is already queued or running",
    ),
    (
        "fk_repositories_routing_rule",
        "routing rule is still used by a repository or does not exist",
    ),
    (
        "fk_repositories_blob_store",
        "blob store reference conflict",
    ),
    (
        "repositories_blob_store_id_fkey",
        "blob store reference conflict",
    ),
    (
        "blobs_blob_store_id_fkey",
        "blob store is still in use or does not exist",
    ),
    (
        "assets_blob_id_fkey",
        "blob is still referenced by an asset",
    ),
    ("assets_repository_id_fkey", "repository does not exist"),
    ("components_repository_id_fkey", "repository does not exist"),
    (
        "privileges_grant_selector_id_fkey",
        "content selector is still referenced by a privilege",
    ),
    (
        "idp_group_mappings_role_id_fkey",
        "role is referenced by a group mapping",
    ),
    (
        "role_privileges_privilege_id_fkey",
        "privilege does not exist",
    ),
    (
        "role_roles_child_role_id_fkey",
        "nested role does not exist",
    ),
    ("user_roles_role_id_fkey", "role does not exist"),
    (
        "ck_repositories_type_config",
        "repository type does not match its configuration",
    ),
    (
        "ck_users_password_hash_format",
        "password must be stored as a hash",
    ),
    (
        "ck_users_hash_only_local",
        "only local accounts have a password hash",
    ),
    (
        "ck_users_oidc_identity",
        "OIDC accounts need an issuer and a subject",
    ),
    (
        "ck_users_local_no_external",
        "local accounts have no external identity",
    ),
    (
        "ck_robot_tokens_name",
        concat!(
            "robot full name must be 1 to 128 letters, digits, '+', '.', '_' or '-', ",
            "starting with a letter or digit"
        ),
    ),
    (
        "ck_robot_tokens_short_name",
        concat!(
            "robot name must be 1 to 64 letters, digits, '.', '_' or '-', ",
            "starting with a letter or digit"
        ),
    ),
    (
        "ck_robot_tokens_scopes",
        "a robot needs at least one scope and at most 64",
    ),
    (
        "ck_robot_tokens_hash",
        "token hash must be 64 lowercase hex characters",
    ),
    (
        "uq_robot_tokens_name",
        "a robot with that name or secret already exists",
    ),
    (
        "uq_robot_tokens_hash",
        "a robot with that name or secret already exists",
    ),
    (
        "uq_project_members_user",
        "the subject already has a role in the project",
    ),
    (
        "uq_project_members_group",
        "the subject already has a role in the project",
    ),
    ("uq_labels_system", "a label with that name already exists"),
    ("uq_labels_project", "a label with that name already exists"),
    (
        "uq_member_single_group",
        "the store already belongs to a group",
    ),
    (
        "uq_member_position",
        "a group already has a member at that position",
    ),
    ("ck_member_not_self", "a store cannot be a member of itself"),
    ("ck_role_roles_not_self", "a role cannot contain itself"),
    (
        "ck_privileges_grant_shape",
        "privilege grant is not valid for this schema version",
    ),
    (
        "privileges_grant_kind_check",
        "privilege grant is not valid for this schema version",
    ),
];

fn constraint_message(constraint: Option<&str>) -> &'static str {
    let Some(name) = constraint else {
        return "a database constraint was violated";
    };
    if let Some((_, message)) = CONSTRAINT_MESSAGES.iter().find(|(known, _)| *known == name) {
        return message;
    }
    if name.ends_with("_pkey") {
        "an entity with this id already exists"
    } else if name.ends_with("_fkey") {
        "referenced entity does not exist or is still in use"
    } else {
        "a database constraint was violated"
    }
}

/// Maps a `sqlx` error raised while doing `context` onto a [`StoreError`] following the rules
/// in the module documentation. Public so that code outside this crate that runs its own
/// `sqlx` statements (a health check, a one-off tool) reports errors the same way.
#[must_use]
pub fn map_err(context: &'static str, err: sqlx::Error) -> StoreError {
    match err {
        sqlx::Error::Database(db) => map_database(context, db.as_ref()),
        sqlx::Error::RowNotFound => StoreError::NotFound("row not found".to_owned()),
        sqlx::Error::PoolTimedOut => {
            StoreError::Unavailable("timed out waiting for a database connection".to_owned())
        }
        sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => {
            StoreError::Unavailable("database connection pool is closed".to_owned())
        }
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) => {
            StoreError::Unavailable("database is unreachable".to_owned())
        }
        other => StoreError::Backend {
            context,
            source: Box::new(DbFault::new(client_side_kind(&other))),
        },
    }
}

/// Coarse description of a client side error. The `sqlx` text is not forwarded because
/// decode errors can include column contents.
fn client_side_kind(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Configuration(_) => "invalid database configuration",
        sqlx::Error::Protocol(_) => "database protocol error",
        sqlx::Error::TypeNotFound { .. }
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::Encode(_) => "row encoding or decoding failed",
        _ => "unexpected database client error",
    }
}

fn map_database(context: &'static str, db: &dyn DatabaseError) -> StoreError {
    let code = db.code().map(std::borrow::Cow::into_owned);
    let code_str = code.as_deref().unwrap_or("");
    match code_str {
        // unique_violation, foreign_key_violation, check_violation, not_null_violation
        "23505" | "23503" | "23514" | "23502" => {
            StoreError::Conflict(constraint_message(db.constraint()).to_owned())
        }
        SQLSTATE_ROLE_CYCLE => StoreError::Conflict("role nesting cycle".to_owned()),
        SQLSTATE_BUILTIN_READONLY => {
            StoreError::Conflict("built-in entity is read-only".to_owned())
        }
        SQLSTATE_AUDIT_APPEND_ONLY => StoreError::Conflict("audit log is append-only".to_owned()),
        SQLSTATE_IMMUTABLE => StoreError::Conflict(IMMUTABLE_TEXT.to_owned()),
        SQLSTATE_HELD => StoreError::Conflict(HELD_TEXT.to_owned()),
        SQLSTATE_QUOTA_EXCEEDED => StoreError::Conflict(QUOTA_TEXT.to_owned()),
        SQLSTATE_READ_ONLY => StoreError::Conflict(READ_ONLY_TEXT.to_owned()),
        SQLSTATE_SCOPE_VIOLATION => StoreError::Conflict(SCOPE_TEXT.to_owned()),
        SQLSTATE_LABEL_LIMIT => {
            StoreError::Invalid("a component carries at most 50 labels".to_owned())
        }
        // serialization_failure, deadlock_detected: the caller may retry the whole operation
        "40001" | "40P01" => StoreError::Unavailable("transaction conflict, retry".to_owned()),
        // query_canceled (statement_timeout), admin_shutdown, crash_shutdown, cannot_connect_now
        "57014" | "57P01" | "57P02" | "57P03" => StoreError::Unavailable(
            "database is shutting down or the statement timed out".to_owned(),
        ),
        // too_many_connections
        "53300" => StoreError::Unavailable("database has no free connection slots".to_owned()),
        c if c.starts_with("08") => {
            StoreError::Unavailable("database connection failed".to_owned())
        }
        _ => StoreError::Backend {
            context,
            source: Box::new(DbFault {
                sqlstate: code,
                message: db.message().to_owned(),
            }),
        },
    }
}

/// The same words as the `Display` of the matching [`PolicyViolation`], so a phase 1 caller
/// (`StoreError::Conflict`) and a typed caller (`MutationError::Violation`) read one text.
const IMMUTABLE_TEXT: &str = "item is immutable";
const HELD_TEXT: &str = "item is under a hold";
const QUOTA_TEXT: &str = "quota exceeded";
const READ_ONLY_TEXT: &str = "read only mode";
const SCOPE_TEXT: &str = "scope violation";

/// DETAIL of a policy guard error, or `None` for any other error.
fn guard_detail(db: &dyn DatabaseError) -> Option<&str> {
    db.try_downcast_ref::<PgDatabaseError>()
        .and_then(PgDatabaseError::detail)
}

/// `<project|repository>:<scope id>:<limit>:<used>:<requested>` of an `RX006` guard.
fn parse_quota_detail(detail: &str) -> Option<QuotaExceeded> {
    let mut parts = detail.splitn(5, ':');
    let kind = parts.next()?;
    let id = Uuid::parse_str(parts.next()?).ok()?;
    let scope = match kind {
        "project" => QuotaScope::Project(ProjectId::from_uuid(id)),
        "repository" => QuotaScope::Repository(RepositoryId::from_uuid(id)),
        _ => return None,
    };
    Some(QuotaExceeded {
        scope,
        limit_bytes: parts.next()?.parse().ok()?,
        used_bytes: parts.next()?.parse().ok()?,
        requested_bytes: parts.next()?.parse().ok()?,
    })
}

/// The typed violation behind a guard error, `None` when the error is not a guard error or its
/// DETAIL does not have the documented shape.
fn violation_of(db: &dyn DatabaseError) -> Option<PolicyViolation> {
    let code = db.code()?;
    let detail = guard_detail(db);
    match code.as_ref() {
        SQLSTATE_IMMUTABLE => Some(PolicyViolation::Immutable {
            rule: ImmutabilityRuleId::from_uuid(Uuid::parse_str(detail?).ok()?),
        }),
        SQLSTATE_HELD => Some(PolicyViolation::Held {
            hold: LifecycleHoldId::from_uuid(Uuid::parse_str(detail?).ok()?),
        }),
        SQLSTATE_QUOTA_EXCEEDED => parse_quota_detail(detail?).map(PolicyViolation::QuotaExceeded),
        SQLSTATE_READ_ONLY => Some(PolicyViolation::ReadOnly),
        _ => None,
    }
}

/// Like [`map_err`] for the typed port methods: a policy guard (`RX004` to `RX007`) becomes
/// [`MutationError::Violation`], everything else a [`MutationError::Store`].
///
/// A guard whose DETAIL cannot be parsed (an older or hand written guard) still reports as a
/// store `Conflict` with the fixed text, never as a backend failure: the write was refused on
/// purpose. `RX008` has no `PolicyViolation` (it is a scope mistake of the caller, not a
/// policy) and stays a `Conflict` in both flavours.
#[must_use]
pub fn map_mutation_err(context: &'static str, err: sqlx::Error) -> MutationError {
    if let sqlx::Error::Database(db) = &err
        && let Some(violation) = violation_of(db.as_ref())
    {
        return MutationError::Violation(violation);
    }
    MutationError::Store(map_err(context, err))
}

/// Wraps a message that is already safe into a `Backend` error.
pub(crate) fn backend(context: &'static str, message: &str) -> StoreError {
    StoreError::Backend {
        context,
        source: Box::new(DbFault::new(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraint_messages_are_static_and_known() {
        assert_eq!(
            constraint_message(Some("uq_users_username_ci")),
            "username already exists"
        );
        assert_eq!(
            constraint_message(Some("things_pkey")),
            "an entity with this id already exists"
        );
        assert_eq!(
            constraint_message(Some("things_other_fkey")),
            "referenced entity does not exist or is still in use"
        );
        assert_eq!(
            constraint_message(None),
            "a database constraint was violated"
        );
    }

    #[test]
    fn pool_and_io_failures_are_unavailable() {
        assert!(matches!(
            map_err("t", sqlx::Error::PoolTimedOut),
            StoreError::Unavailable(_)
        ));
        assert!(matches!(
            map_err("t", sqlx::Error::PoolClosed),
            StoreError::Unavailable(_)
        ));
        let io = std::io::Error::other("connection reset by peer: secret-host");
        let mapped = map_err("t", sqlx::Error::Io(io));
        assert!(matches!(&mapped, StoreError::Unavailable(m) if !m.contains("secret-host")));
        assert!(matches!(
            map_err("t", sqlx::Error::RowNotFound),
            StoreError::NotFound(_)
        ));
    }

    #[test]
    fn must_not_forward_client_side_error_text() {
        let err = sqlx::Error::Protocol("column value 'hunter2' unexpected".to_owned());
        let mapped = map_err("read user", err);
        let shown = format!("{mapped} {mapped:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(matches!(mapped, StoreError::Backend { .. }));
    }

    #[test]
    fn quota_details_are_parsed_strictly() {
        let id = Uuid::now_v7();
        let parsed = parse_quota_detail(&format!("repository:{id}:1000:900:200")).unwrap();
        assert_eq!(
            parsed.scope,
            QuotaScope::Repository(RepositoryId::from_uuid(id))
        );
        assert_eq!(
            (
                parsed.limit_bytes,
                parsed.used_bytes,
                parsed.requested_bytes
            ),
            (1000, 900, 200)
        );
        let project = parse_quota_detail(&format!("project:{id}:1:2:3")).unwrap();
        assert_eq!(project.scope, QuotaScope::Project(ProjectId::from_uuid(id)));
        for bad in [
            "",
            "project",
            "project:not-a-uuid:1:2:3",
            &format!("galaxy:{id}:1:2:3"),
            &format!("project:{id}:1:2"),
            &format!("project:{id}:-1:2:3"),
            &format!("project:{id}:1:two:3"),
            &format!("project:{id}:1:2:3:4"),
        ] {
            assert!(parse_quota_detail(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn guard_texts_match_the_violation_display() {
        // A phase 1 caller and a typed caller read one text for one refusal.
        assert_eq!(
            IMMUTABLE_TEXT,
            PolicyViolation::Immutable {
                rule: ImmutabilityRuleId::generate()
            }
            .to_string()
        );
        assert_eq!(
            HELD_TEXT,
            PolicyViolation::Held {
                hold: LifecycleHoldId::generate()
            }
            .to_string()
        );
        assert_eq!(READ_ONLY_TEXT, PolicyViolation::ReadOnly.to_string());
        let quota = QuotaExceeded {
            scope: QuotaScope::Project(ProjectId::generate()),
            limit_bytes: 1,
            used_bytes: 1,
            requested_bytes: 1,
        };
        assert_eq!(
            QUOTA_TEXT,
            PolicyViolation::QuotaExceeded(quota).to_string()
        );
    }

    #[test]
    fn a_non_database_error_is_never_a_violation() {
        let mapped = map_mutation_err("t", sqlx::Error::PoolTimedOut);
        assert!(matches!(
            mapped,
            MutationError::Store(StoreError::Unavailable(_))
        ));
    }
}
