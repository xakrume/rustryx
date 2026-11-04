//! [`ProjectMemberRepo`] and [`ProjectAccessRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::IdpProvider;
use rustryx_core::ids::{ProjectId, ProjectMemberId, UserId};
use rustryx_core::ports::{
    InvalidationEvent, Page, PageRequest, ProjectAccessRepo, ProjectMemberRepo,
};
use rustryx_core::project::{ProjectMember, ProjectRole, ProjectSubject};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, notify};
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "project_member";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, project_id, subject_kind, user_id, provider, external_group, role, \
             added_by, created_at FROM project_members ",
            $($tail),*
        )
    };
}

/// Project membership rows.
#[derive(Debug, Clone)]
pub struct PgProjectMemberRepo {
    pool: PgPool,
}

impl PgProjectMemberRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Direct memberships and group rows read by RBAC.
#[derive(Debug, Clone)]
pub struct PgProjectAccessRepo {
    pool: PgPool,
}

impl PgProjectAccessRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const fn provider_label(provider: IdpProvider) -> &'static str {
    match provider {
        IdpProvider::Oidc => "oidc",
        IdpProvider::Ldap => "ldap",
    }
}

fn parse_provider(label: &str) -> Result<IdpProvider, StoreError> {
    match label {
        "oidc" => Ok(IdpProvider::Oidc),
        "ldap" => Ok(IdpProvider::Ldap),
        _ => Err(corrupt("read project member")),
    }
}

fn parse_role(label: &str) -> Result<ProjectRole, StoreError> {
    ProjectRole::parse(label).map_err(|_| corrupt("read project member"))
}

fn missing(id: ProjectMemberId) -> StoreError {
    StoreError::NotFound(format!("project member {id}"))
}

fn subject_event(subject: &ProjectSubject) -> InvalidationEvent {
    match subject {
        ProjectSubject::User { user_id } => InvalidationEvent::User(user_id.as_uuid()),
        ProjectSubject::Group { .. } => InvalidationEvent::Projects,
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    project_id: Uuid,
    subject_kind: String,
    user_id: Option<Uuid>,
    provider: Option<String>,
    external_group: Option<String>,
    role: String,
    added_by: Option<Uuid>,
    created_at: OffsetDateTime,
}

impl Row {
    fn into_member(self) -> Result<ProjectMember, StoreError> {
        let subject = match self.subject_kind.as_str() {
            "user" => ProjectSubject::User {
                user_id: UserId::from_uuid(
                    self.user_id
                        .ok_or_else(|| corrupt("read project member"))?,
                ),
            },
            "group" => ProjectSubject::Group {
                provider: parse_provider(
                    self.provider
                        .as_deref()
                        .ok_or_else(|| corrupt("read project member"))?,
                )?,
                external_group: self
                    .external_group
                    .ok_or_else(|| corrupt("read project member"))?,
            },
            _ => return Err(corrupt("read project member")),
        };
        let mut member = ProjectMember::new(
            ProjectMemberId::from_uuid(self.id),
            ProjectId::from_uuid(self.project_id),
            subject,
            parse_role(&self.role)?,
            self.created_at,
        );
        member.added_by = self.added_by.map(UserId::from_uuid);
        Ok(member)
    }
}

#[derive(Debug, FromRow)]
struct SubjectRow {
    subject_kind: String,
    user_id: Option<Uuid>,
}

fn event_of_row(row: &SubjectRow) -> Result<InvalidationEvent, StoreError> {
    match row.subject_kind.as_str() {
        "user" => Ok(InvalidationEvent::User(
            row.user_id.ok_or_else(|| corrupt("read project member"))?,
        )),
        "group" => Ok(InvalidationEvent::Projects),
        _ => Err(corrupt("read project member")),
    }
}

async fn allow_last_admin(
    tx: &mut sqlx::PgConnection,
    allow: bool,
    context: &'static str,
) -> Result<(), StoreError> {
    if !allow {
        return Ok(());
    }
    let _: String = sqlx::query_scalar("SELECT set_config('rustryx.allow_last_admin', 'on', true)")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err(context, e))?;
    Ok(())
}

struct SubjectCols<'a> {
    kind: &'static str,
    user_id: Option<Uuid>,
    provider: Option<&'static str>,
    group: Option<&'a str>,
}

fn subject_columns(subject: &ProjectSubject) -> SubjectCols<'_> {
    match subject {
        ProjectSubject::User { user_id } => SubjectCols {
            kind: "user",
            user_id: Some(user_id.as_uuid()),
            provider: None,
            group: None,
        },
        ProjectSubject::Group {
            provider,
            external_group,
        } => SubjectCols {
            kind: "group",
            user_id: None,
            provider: Some(provider_label(*provider)),
            group: Some(external_group.as_str()),
        },
    }
}

async fn insert_member(pool: &PgPool, member: &ProjectMember) -> Result<(), StoreError> {
    let cols = subject_columns(&member.subject);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| map_err("add project member", e))?;
    sqlx::query(
        "INSERT INTO project_members (id, project_id, subject_kind, user_id, provider, \
         external_group, role, added_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(member.id.as_uuid())
    .bind(member.project_id.as_uuid())
    .bind(cols.kind)
    .bind(cols.user_id)
    .bind(cols.provider)
    .bind(cols.group)
    .bind(member.role.as_str())
    .bind(member.added_by.map(|id| id.as_uuid()))
    .bind(member.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_err("add project member", e))?;
    notify(&mut tx, &subject_event(&member.subject)).await?;
    tx.commit()
        .await
        .map_err(|e| map_err("add project member", e))
}

#[async_trait]
impl ProjectMemberRepo for PgProjectMemberRepo {
    async fn list(
        &self,
        project: ProjectId,
        page: &PageRequest,
    ) -> Result<Page<ProjectMember>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(
                Uuid::parse_str(&cursor::decode(KIND, token)?)
                    .map_err(|_| cursor::invalid_token())?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE project_id = $1 AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3"
        ))
        .bind(project.as_uuid())
        .bind(after)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list project members", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_member)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |member| {
            member.id.to_string()
        }))
    }

    async fn get(&self, id: ProjectMemberId) -> Result<Option<ProjectMember>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read project member", e))?
            .map(Row::into_member)
            .transpose()
    }

    async fn add(&self, member: &ProjectMember) -> Result<(), StoreError> {
        member
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        insert_member(&self.pool, member).await
    }

    async fn set_role(
        &self,
        id: ProjectMemberId,
        role: ProjectRole,
        allow_last_admin_flag: bool,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("set project member role", e))?;
        allow_last_admin(&mut tx, allow_last_admin_flag, "set project member role").await?;
        let row = sqlx::query_as::<_, SubjectRow>(
            "UPDATE project_members SET role = $2 WHERE id = $1 RETURNING subject_kind, user_id",
        )
        .bind(id.as_uuid())
        .bind(role.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("set project member role", e))?;
        let Some(row) = row else {
            return Err(missing(id));
        };
        notify(&mut tx, &event_of_row(&row)?).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("set project member role", e))
    }

    async fn remove(
        &self,
        id: ProjectMemberId,
        allow_last_admin_flag: bool,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("remove project member", e))?;
        allow_last_admin(&mut tx, allow_last_admin_flag, "remove project member").await?;
        let row = sqlx::query_as::<_, SubjectRow>(
            "DELETE FROM project_members WHERE id = $1 RETURNING subject_kind, user_id",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("remove project member", e))?;
        let Some(row) = row else {
            return Err(missing(id));
        };
        notify(&mut tx, &event_of_row(&row)?).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("remove project member", e))
    }
}

#[derive(Debug, FromRow)]
struct AccessRow {
    project_id: Uuid,
    role: String,
}

#[derive(Debug, FromRow)]
struct GroupRow {
    external_group: String,
    project_id: Uuid,
    role: String,
}

#[async_trait]
impl ProjectAccessRepo for PgProjectAccessRepo {
    async fn memberships_of_user(
        &self,
        user: UserId,
    ) -> Result<Vec<(ProjectId, ProjectRole)>, StoreError> {
        let rows = sqlx::query_as::<_, AccessRow>(
            "SELECT project_id, role FROM project_members \
             WHERE subject_kind = 'user' AND user_id = $1",
        )
        .bind(user.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read project memberships", e))?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    ProjectId::from_uuid(row.project_id),
                    parse_role(&row.role)?,
                ))
            })
            .collect()
    }

    async fn group_mappings(
        &self,
        provider: IdpProvider,
        limit: u32,
    ) -> Result<Vec<(String, ProjectId, ProjectRole)>, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM project_members \
             WHERE subject_kind = 'group' AND provider = $1",
        )
        .bind(provider_label(provider))
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("read project groups", e))?;
        if count > i64::from(limit) {
            return Err(StoreError::Invalid(format!(
                "more than {limit} project group rows"
            )));
        }
        let rows = sqlx::query_as::<_, GroupRow>(
            "SELECT external_group, project_id, role FROM project_members \
             WHERE subject_kind = 'group' AND provider = $1",
        )
        .bind(provider_label(provider))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read project groups", e))?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.external_group,
                    ProjectId::from_uuid(row.project_id),
                    parse_role(&row.role)?,
                ))
            })
            .collect()
    }
}
