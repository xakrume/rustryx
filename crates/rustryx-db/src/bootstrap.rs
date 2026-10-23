//! Creation of the first administrator.
//!
//! The schema seeds roles but no user and no password. The deployment hands the process a
//! password HASH (the secret itself is hashed by `rustryx-auth`), and this function stores it
//! only when the instance has no administrator yet.

use rustryx_core::identity::ROLE_NX_ADMIN;
use rustryx_core::ids::UserId;
use rustryx_core::ports::InvalidationEvent;
use rustryx_core::{Secret, StoreError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::invalidation;

/// Creates a local administrator `username` with `password_hash` when no user holds
/// `nx-admin` yet. Returns `true` if the account was created, `false` if an administrator
/// already exists (in that case nothing is written and the hash is ignored).
///
/// - Any `nx-admin` assignment counts, whatever its origin or whether the account is enabled:
///   an existing administrator is repaired with the user tooling, not shadowed by a second
///   account created on every start.
/// - `password_hash` must be a PHC argon2 string or a bcrypt hash. The `users` constraint
///   refuses anything else, so passing the password itself fails with `Conflict` instead of
///   storing it.
/// - Concurrent callers (two nodes starting together) are serialised by a transaction level
///   advisory lock, so exactly one of them creates the account.
/// - `Conflict` when `username` is taken by a non-administrator.
pub async fn bootstrap_admin(
    pool: &PgPool,
    username: &str,
    password_hash: &Secret,
) -> Result<bool, StoreError> {
    let context = "bootstrap admin";
    let mut tx = pool.begin().await.map_err(|e| map_err(context, e))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rustryx.bootstrap_admin', 0))")
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err(context, e))?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.name = $1)",
    )
    .bind(ROLE_NX_ADMIN)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| map_err(context, e))?;
    if exists {
        return Ok(false);
    }
    let id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, source, password_hash) VALUES ($1, $2, 'local', $3)",
    )
    .bind(id.as_uuid())
    .bind(username)
    .bind(password_hash.expose())
    .execute(&mut *tx)
    .await
    .map_err(|e| map_err(context, e))?;
    let granted = sqlx::query(
        "INSERT INTO user_roles (user_id, role_id, origin) \
         SELECT $1, id, 'manual' FROM roles WHERE name = $2",
    )
    .bind(id.as_uuid())
    .bind(ROLE_NX_ADMIN)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_err(context, e))?;
    if granted.rows_affected() != 1 {
        // The built-in role is missing: the schema is damaged. Nothing is committed.
        return Err(crate::error::backend(
            context,
            "built-in administrator role is missing",
        ));
    }
    invalidation::send(&mut tx, &InvalidationEvent::User(id.as_uuid())).await?;
    tx.commit().await.map_err(|e| map_err(context, e))?;
    Ok(true)
}
