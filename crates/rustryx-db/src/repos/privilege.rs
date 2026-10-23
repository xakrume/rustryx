//! [`PrivilegeRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::{Action, Privilege, PrivilegeGrant};
use rustryx_core::ids::{ContentSelectorId, PrivilegeId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{InvalidationEvent, PrivilegeRepo};
use sqlx::{FromRow, PgPool};
use std::str::FromStr;
use uuid::Uuid;

use super::common::{corrupt, notify};
use crate::error::map_err;

/// Columns of [`PrivilegeRow`], for statements that alias the table as `p` or not at all.
macro_rules! privilege_select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, description, builtin, grant_kind, grant_format, grant_repository, \
             grant_selector_id, actions FROM privileges ",
            $($tail),*
        )
    };
}

/// Privileges.
#[derive(Debug, Clone)]
pub struct PgPrivilegeRepo {
    pool: PgPool,
}

impl PgPrivilegeRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const fn action_label(action: Action) -> &'static str {
    match action {
        Action::Browse => "browse",
        Action::Read => "read",
        Action::Write => "write",
        Action::Delete => "delete",
    }
}

fn parse_action(label: &str) -> Result<Action, StoreError> {
    match label {
        "browse" => Ok(Action::Browse),
        "read" => Ok(Action::Read),
        "write" => Ok(Action::Write),
        "delete" => Ok(Action::Delete),
        _ => Err(corrupt("read privilege")),
    }
}

/// The grant flattened into the columns of `privileges`.
struct GrantColumns {
    kind: &'static str,
    format: Option<&'static str>,
    repository: Option<String>,
    selector_id: Option<Uuid>,
}

fn grant_columns(grant: &PrivilegeGrant) -> GrantColumns {
    match grant {
        PrivilegeGrant::Admin => GrantColumns {
            kind: "admin",
            format: None,
            repository: None,
            selector_id: None,
        },
        PrivilegeGrant::RepositoryView { format, repository } => GrantColumns {
            kind: "repository-view",
            format: format.map(RepoFormat::as_str),
            repository: Some(repository.clone()),
            selector_id: None,
        },
        PrivilegeGrant::RepositoryContentSelector {
            repository,
            selector_id,
        } => GrantColumns {
            kind: "repository-content-selector",
            format: None,
            repository: Some(repository.clone()),
            selector_id: Some(selector_id.as_uuid()),
        },
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PrivilegeRow {
    id: Uuid,
    name: String,
    description: String,
    builtin: bool,
    grant_kind: String,
    grant_format: Option<String>,
    grant_repository: Option<String>,
    grant_selector_id: Option<Uuid>,
    actions: Vec<String>,
}

impl PrivilegeRow {
    pub(crate) fn into_privilege(self) -> Result<Privilege, StoreError> {
        let grant = match (
            self.grant_kind.as_str(),
            self.grant_repository,
            self.grant_selector_id,
        ) {
            ("admin", None, None) => PrivilegeGrant::Admin,
            ("repository-view", Some(repository), None) => {
                let format = self
                    .grant_format
                    .as_deref()
                    .map(RepoFormat::from_str)
                    .transpose()
                    .map_err(|_| corrupt("read privilege"))?;
                PrivilegeGrant::RepositoryView { format, repository }
            }
            ("repository-content-selector", Some(repository), Some(selector)) => {
                PrivilegeGrant::RepositoryContentSelector {
                    repository,
                    selector_id: ContentSelectorId::from_uuid(selector),
                }
            }
            _ => return Err(corrupt("read privilege")),
        };
        let actions = self
            .actions
            .iter()
            .map(|a| parse_action(a))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Privilege {
            id: PrivilegeId::from_uuid(self.id),
            name: self.name,
            description: self.description,
            builtin: self.builtin,
            grant,
            actions,
        })
    }
}

fn action_labels(privilege: &Privilege) -> Vec<&'static str> {
    let mut labels: Vec<&'static str> =
        privilege.actions.iter().map(|a| action_label(*a)).collect();
    labels.sort_unstable();
    labels.dedup();
    labels
}

#[async_trait]
impl PrivilegeRepo for PgPrivilegeRepo {
    async fn get(&self, id: PrivilegeId) -> Result<Option<Privilege>, StoreError> {
        sqlx::query_as::<_, PrivilegeRow>(privilege_select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read privilege", e))?
            .map(PrivilegeRow::into_privilege)
            .transpose()
    }

    async fn list(&self) -> Result<Vec<Privilege>, StoreError> {
        sqlx::query_as::<_, PrivilegeRow>(privilege_select!("ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list privileges", e))?
            .into_iter()
            .map(PrivilegeRow::into_privilege)
            .collect()
    }

    /// `builtin` is owned by the migrations: a privilege created here is never built-in,
    /// whatever the struct says, so an API caller cannot mint an undeletable privilege.
    async fn create(&self, privilege: &Privilege) -> Result<(), StoreError> {
        let g = grant_columns(&privilege.grant);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create privilege", e))?;
        sqlx::query(
            "INSERT INTO privileges (id, name, description, builtin, grant_kind, grant_format, \
             grant_repository, grant_selector_id, actions) \
             VALUES ($1, $2, $3, false, $4, $5, $6, $7, $8)",
        )
        .bind(privilege.id.as_uuid())
        .bind(&privilege.name)
        .bind(&privilege.description)
        .bind(g.kind)
        .bind(g.format)
        .bind(&g.repository)
        .bind(g.selector_id)
        .bind(action_labels(privilege))
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("create privilege", e))?;
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("create privilege", e))
    }

    /// A built-in privilege is refused by the `privileges` trigger (`Conflict`).
    async fn update(&self, privilege: &Privilege) -> Result<(), StoreError> {
        let g = grant_columns(&privilege.grant);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update privilege", e))?;
        // `name` stays out of this SET list unless it changes: a unique-index column in the
        // list turns the row lock into a KEY lock that blocks the foreign key checks of
        // concurrent role_privileges inserts (see the note in `role.rs`).
        let current_name: Option<String> = sqlx::query_scalar(
            "UPDATE privileges SET description = $2, grant_kind = $3, grant_format = $4, \
             grant_repository = $5, grant_selector_id = $6, actions = $7, updated_at = now() \
             WHERE id = $1 RETURNING name",
        )
        .bind(privilege.id.as_uuid())
        .bind(&privilege.description)
        .bind(g.kind)
        .bind(g.format)
        .bind(&g.repository)
        .bind(g.selector_id)
        .bind(action_labels(privilege))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("update privilege", e))?;
        let Some(current_name) = current_name else {
            return Err(StoreError::NotFound("privilege".to_owned()));
        };
        if current_name != privilege.name {
            sqlx::query("UPDATE privileges SET name = $2 WHERE id = $1")
                .bind(privilege.id.as_uuid())
                .bind(&privilege.name)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("rename privilege", e))?;
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("update privilege", e))
    }

    /// Removes the privilege from every role (cascade). That only narrows access, which is
    /// the safe direction. A built-in privilege is refused by trigger.
    async fn delete(&self, id: PrivilegeId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete privilege", e))?;
        let result = sqlx::query("DELETE FROM privileges WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete privilege", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("privilege".to_owned()));
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete privilege", e))
    }
}
