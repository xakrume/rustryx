//! [`RoleRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::{Privilege, Role};
use rustryx_core::ids::{PrivilegeId, RoleId};
use rustryx_core::ports::{InvalidationEvent, RoleRepo};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::common::notify;
use super::privilege::PrivilegeRow;
use crate::error::map_err;

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT r.id, r.name, r.description, r.builtin, \
             ARRAY(SELECT rp.privilege_id FROM role_privileges rp WHERE rp.role_id = r.id \
                   ORDER BY rp.privilege_id) AS privilege_ids, \
             ARRAY(SELECT rr.child_role_id FROM role_roles rr WHERE rr.parent_role_id = r.id \
                   ORDER BY rr.child_role_id) AS nested_role_ids \
             FROM roles r ",
            $($tail),*
        )
    };
}

/// Roles.
#[derive(Debug, Clone)]
pub struct PgRoleRepo {
    pool: PgPool,
}

impl PgRoleRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    name: String,
    description: String,
    builtin: bool,
    privilege_ids: Vec<Uuid>,
    nested_role_ids: Vec<Uuid>,
}

impl From<Row> for Role {
    fn from(row: Row) -> Self {
        Self {
            id: RoleId::from_uuid(row.id),
            name: row.name,
            description: row.description,
            builtin: row.builtin,
            privilege_ids: row
                .privilege_ids
                .into_iter()
                .map(PrivilegeId::from_uuid)
                .collect(),
            nested_role_ids: row
                .nested_role_ids
                .into_iter()
                .map(RoleId::from_uuid)
                .collect(),
        }
    }
}

fn distinct<T: Copy + Ord>(values: &[T], key: impl Fn(T) -> Uuid) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = values.iter().map(|v| key(*v)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Serialises every write to the role graph for the rest of the transaction.
///
/// It is the FIRST statement of each role write, before any row is touched. The cycle guard
/// trigger takes the same lock, but only when a nesting row is inserted, which is after the
/// role row was updated; two writers taking their locks in different orders (row first, then
/// this lock) deadlocked in testing. Taking it up front gives every writer one lock order.
/// Re-taking it in the trigger is harmless: an advisory transaction lock is re-entrant.
async fn lock_role_graph(conn: &mut PgConnection, context: &'static str) -> Result<(), StoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rustryx.role_roles', 0))")
        .execute(conn)
        .await
        .map_err(|e| map_err(context, e))?;
    Ok(())
}

/// Replaces the privilege and nesting rows of a role. The nesting insert fires the cycle
/// guard trigger, which serialises all nesting writes (see migration 3).
async fn write_members(
    conn: &mut PgConnection,
    role: &Role,
    context: &'static str,
) -> Result<(), StoreError> {
    let privileges = distinct(&role.privilege_ids, |p| p.as_uuid());
    let nested = distinct(&role.nested_role_ids, |r| r.as_uuid());
    sqlx::query("DELETE FROM role_privileges WHERE role_id = $1")
        .bind(role.id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(|e| map_err(context, e))?;
    sqlx::query("DELETE FROM role_roles WHERE parent_role_id = $1")
        .bind(role.id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(|e| map_err(context, e))?;
    sqlx::query("INSERT INTO role_privileges (role_id, privilege_id) SELECT $1, p FROM unnest($2::uuid[]) AS p")
        .bind(role.id.as_uuid())
        .bind(&privileges)
        .execute(&mut *conn)
        .await
        .map_err(|e| map_err(context, e))?;
    sqlx::query("INSERT INTO role_roles (parent_role_id, child_role_id) SELECT $1, c FROM unnest($2::uuid[]) AS c")
        .bind(role.id.as_uuid())
        .bind(&nested)
        .execute(&mut *conn)
        .await
        .map_err(|e| map_err(context, e))?;
    Ok(())
}

#[async_trait]
impl RoleRepo for PgRoleRepo {
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE r.id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read role", e))?
            .map(Role::from))
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE r.name = $1"))
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read role", e))?
            .map(Role::from))
    }

    async fn list(&self) -> Result<Vec<Role>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("ORDER BY r.name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list roles", e))?
            .into_iter()
            .map(Role::from)
            .collect())
    }

    /// `builtin` is owned by the migrations, so a role created here is never built-in. A
    /// nesting cycle or a duplicate name is `Conflict`; the whole role (row, privileges,
    /// nesting) is one transaction.
    async fn create(&self, role: &Role) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create role", e))?;
        lock_role_graph(&mut tx, "create role").await?;
        sqlx::query(
            "INSERT INTO roles (id, name, description, builtin) VALUES ($1, $2, $3, false)",
        )
        .bind(role.id.as_uuid())
        .bind(&role.name)
        .bind(&role.description)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("create role", e))?;
        write_members(&mut tx, role, "create role").await?;
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("create role", e))
    }

    /// `NotFound` when absent; `Conflict` for a built-in role (trigger) or a nesting cycle.
    /// The `UPDATE` of the role row comes first and takes its row lock, so concurrent updates
    /// of one role run one after the other.
    async fn update(&self, role: &Role) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update role", e))?;
        lock_role_graph(&mut tx, "update role").await?;
        // `name` is not in this SET list on purpose. A column of a unique index in the SET
        // list makes PostgreSQL take the row lock as a KEY update even when the value is
        // unchanged, and that lock blocks the foreign key checks of every concurrent insert
        // into user_roles, role_privileges or role_roles that points at the role.
        let current_name: Option<String> = sqlx::query_scalar(
            "UPDATE roles SET description = $2, updated_at = now() WHERE id = $1 RETURNING name",
        )
        .bind(role.id.as_uuid())
        .bind(&role.description)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("update role", e))?;
        let Some(current_name) = current_name else {
            return Err(StoreError::NotFound("role".to_owned()));
        };
        if current_name != role.name {
            sqlx::query("UPDATE roles SET name = $2 WHERE id = $1")
                .bind(role.id.as_uuid())
                .bind(&role.name)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("rename role", e))?;
        }
        write_members(&mut tx, role, "update role").await?;
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("update role", e))
    }

    /// Removes the role from every user (cascade). `Conflict` while a group mapping still
    /// points at it, or for a built-in role.
    async fn delete(&self, id: RoleId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete role", e))?;
        lock_role_graph(&mut tx, "delete role").await?;
        let result = sqlx::query("DELETE FROM roles WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete role", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("role".to_owned()));
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("delete role", e))
    }

    /// Recursive walk over `role_roles`. `UNION` (not `UNION ALL`) drops rows already seen,
    /// so the recursion ends even on a cycle that a hand edit managed to create.
    async fn effective_privileges(&self, roles: &[RoleId]) -> Result<Vec<Privilege>, StoreError> {
        if roles.is_empty() {
            return Ok(Vec::new());
        }
        let start: Vec<Uuid> = roles.iter().map(RoleId::as_uuid).collect();
        sqlx::query_as::<_, PrivilegeRow>(
            "WITH RECURSIVE reach(role_id) AS ( \
                 SELECT unnest($1::uuid[]) \
                 UNION \
                 SELECT rr.child_role_id FROM role_roles rr JOIN reach r ON rr.parent_role_id = r.role_id \
             ) \
             SELECT p.id, p.name, p.description, p.builtin, p.grant_kind, p.grant_format, \
                    p.grant_repository, p.grant_selector_id, p.actions \
             FROM privileges p \
             WHERE p.id IN (SELECT rp.privilege_id FROM role_privileges rp \
                            WHERE rp.role_id IN (SELECT role_id FROM reach)) \
             ORDER BY p.name",
        )
        .bind(&start)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("resolve effective privileges", e))?
        .into_iter()
        .map(PrivilegeRow::into_privilege)
        .collect()
    }
}
