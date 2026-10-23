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

use std::fmt;

use rustryx_core::StoreError;
use sqlx::error::DatabaseError;

/// SQLSTATE raised by the role nesting guard.
pub const SQLSTATE_ROLE_CYCLE: &str = "RX001";
/// SQLSTATE raised when a built-in role or privilege is modified.
pub const SQLSTATE_BUILTIN_READONLY: &str = "RX002";
/// SQLSTATE raised when the append-only audit table is modified.
pub const SQLSTATE_AUDIT_APPEND_ONLY: &str = "RX003";

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
    (
        "fk_repositories_blob_store",
        "blob store reference conflict",
    ),
    (
        "repositories_blob_store_id_fkey",
        "blob store reference conflict",
    ),
    ("blobs_blob_store_id_fkey", "blob store is still in use"),
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
    ("ck_role_roles_not_self", "a role cannot contain itself"),
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
}
