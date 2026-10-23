//! [`UserRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::Secret;
use rustryx_core::StoreError;
use rustryx_core::identity::{NewUser, RoleOrigin, RoleRef, User, UserSource};
use rustryx_core::ids::{RoleId, UserId};
use rustryx_core::ports::{InvalidationEvent, Page, PageRequest, UserRepo};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, notify};
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "user";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, username, source, issuer, subject, email, display_name, enabled, \
             password_hash, tokens_valid_after, created_at FROM users ",
            $($tail),*
        )
    };
}

/// Users and their role assignments.
#[derive(Debug, Clone)]
pub struct PgUserRepo {
    pool: PgPool,
}

impl PgUserRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

pub(crate) const fn source_label(source: UserSource) -> &'static str {
    match source {
        UserSource::Local => "local",
        UserSource::Ldap => "ldap",
        UserSource::Oidc => "oidc",
    }
}

fn parse_source(label: &str) -> Result<UserSource, StoreError> {
    match label {
        "local" => Ok(UserSource::Local),
        "ldap" => Ok(UserSource::Ldap),
        "oidc" => Ok(UserSource::Oidc),
        _ => Err(corrupt("read user")),
    }
}

pub(crate) const fn origin_label(origin: RoleOrigin) -> &'static str {
    match origin {
        RoleOrigin::Manual => "manual",
        RoleOrigin::Oidc => "oidc",
        RoleOrigin::Ldap => "ldap",
    }
}

fn parse_origin(label: &str) -> Result<RoleOrigin, StoreError> {
    match label {
        "manual" => Ok(RoleOrigin::Manual),
        "oidc" => Ok(RoleOrigin::Oidc),
        "ldap" => Ok(RoleOrigin::Ldap),
        _ => Err(corrupt("read user roles")),
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    username: String,
    source: String,
    issuer: Option<String>,
    subject: Option<String>,
    email: Option<String>,
    display_name: Option<String>,
    enabled: bool,
    password_hash: Option<String>,
    tokens_valid_after: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl Row {
    fn into_user(self) -> Result<User, StoreError> {
        Ok(User {
            id: UserId::from_uuid(self.id),
            username: self.username,
            source: parse_source(&self.source)?,
            issuer: self.issuer,
            subject: self.subject,
            email: self.email,
            display_name: self.display_name,
            enabled: self.enabled,
            password_hash: self.password_hash.map(Secret::new),
            tokens_valid_after: self.tokens_valid_after,
            created_at: self.created_at,
        })
    }
}

async fn one(
    pool: &PgPool,
    query: sqlx::query::QueryAs<'_, sqlx::Postgres, Row, sqlx::postgres::PgArguments>,
    context: &'static str,
) -> Result<Option<User>, StoreError> {
    query
        .fetch_optional(pool)
        .await
        .map_err(|e| map_err(context, e))?
        .map(Row::into_user)
        .transpose()
}

/// Runs a single-row `UPDATE` that must hit exactly one user, then announces the change.
async fn update_one(
    pool: &PgPool,
    statement: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    id: UserId,
    context: &'static str,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(|e| map_err(context, e))?;
    let result = statement
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err(context, e))?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound("user".to_owned()));
    }
    notify(&mut tx, &InvalidationEvent::User(id.as_uuid())).await?;
    tx.commit().await.map_err(|e| map_err(context, e))
}

#[async_trait]
impl UserRepo for PgUserRepo {
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError> {
        one(
            &self.pool,
            sqlx::query_as::<_, Row>(select!("WHERE id = $1")).bind(id.as_uuid()),
            "read user",
        )
        .await
    }

    /// Case-insensitive: usernames are unique on `lower(username)`, so the lookup uses the
    /// same expression and the same index.
    async fn find_by_username(&self, username: &str) -> Result<Option<User>, StoreError> {
        one(
            &self.pool,
            sqlx::query_as::<_, Row>(select!("WHERE lower(username) = lower($1)")).bind(username),
            "read user by name",
        )
        .await
    }

    /// Matches `(source, issuer, subject)` and nothing else. Without a subject there is no
    /// external identity to look for, so the answer is `None` instead of a query that could
    /// match on a missing value.
    async fn find_external(
        &self,
        source: UserSource,
        issuer: Option<&str>,
        subject: Option<&str>,
    ) -> Result<Option<User>, StoreError> {
        let Some(subject) = subject else {
            return Ok(None);
        };
        one(
            &self.pool,
            sqlx::query_as::<_, Row>(select!(
                "WHERE source = $1 AND coalesce(issuer, '') = coalesce($2, '') AND subject = $3"
            ))
            .bind(source_label(source))
            .bind(issuer)
            .bind(subject),
            "read external user",
        )
        .await
    }

    async fn create(&self, new: &NewUser) -> Result<User, StoreError> {
        sqlx::query_as::<_, Row>(
            "INSERT INTO users (id, username, source, issuer, subject, email, display_name, \
             password_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING id, username, source, issuer, subject, email, display_name, enabled, \
             password_hash, tokens_valid_after, created_at",
        )
        .bind(UserId::generate().as_uuid())
        .bind(&new.username)
        .bind(source_label(new.source))
        .bind(&new.issuer)
        .bind(&new.subject)
        .bind(&new.email)
        .bind(&new.display_name)
        .bind(new.password_hash.as_ref().map(Secret::expose))
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("create user", e))?
        .into_user()
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<User>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(
                Uuid::parse_str(&cursor::decode(KIND, token)?)
                    .map_err(|_| StoreError::Conflict("invalid page token".to_owned()))?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT $2"
        ))
        .bind(after)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list users", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_user)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |u| u.id.to_string()))
    }

    /// The `users` constraints refuse anything that is not a PHC argon2 or bcrypt string
    /// and refuse a hash on a non-local account (`Conflict`).
    async fn set_password_hash(&self, id: UserId, hash: &str) -> Result<(), StoreError> {
        update_one(
            &self.pool,
            sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
                .bind(id.as_uuid())
                .bind(hash),
            id,
            "set password hash",
        )
        .await
    }

    async fn set_enabled(&self, id: UserId, enabled: bool) -> Result<(), StoreError> {
        update_one(
            &self.pool,
            sqlx::query("UPDATE users SET enabled = $2 WHERE id = $1")
                .bind(id.as_uuid())
                .bind(enabled),
            id,
            "set user enabled",
        )
        .await
    }

    async fn set_tokens_valid_after(
        &self,
        id: UserId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        update_one(
            &self.pool,
            sqlx::query("UPDATE users SET tokens_valid_after = $2 WHERE id = $1")
                .bind(id.as_uuid())
                .bind(at),
            id,
            "set tokens valid after",
        )
        .await
    }

    async fn roles_of(&self, id: UserId) -> Result<Vec<(RoleRef, RoleOrigin)>, StoreError> {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT r.id, r.name, ur.origin FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
             WHERE ur.user_id = $1 ORDER BY r.name, ur.origin",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read user roles", e))?;
        rows.into_iter()
            .map(|(role_id, name, origin)| {
                Ok((
                    RoleRef {
                        id: RoleId::from_uuid(role_id),
                        name,
                    },
                    parse_origin(&origin)?,
                ))
            })
            .collect()
    }

    /// One transaction: lock the user row (concurrent syncs of one user run one after the
    /// other), delete this origin's rows that are not in `roles`, insert the missing ones,
    /// queue the notification. Rows of other origins are never read or written, which is
    /// what keeps a manual grant alive when a Keycloak group changes. Always notifies, even
    /// when nothing changed: an extra cache drop is harmless and the callers throttle syncs.
    async fn replace_roles_for_origin(
        &self,
        id: UserId,
        origin: RoleOrigin,
        roles: &[RoleId],
    ) -> Result<(), StoreError> {
        let context = "replace user roles";
        let wanted: Vec<Uuid> = roles.iter().map(RoleId::as_uuid).collect();
        let mut tx = self.pool.begin().await.map_err(|e| map_err(context, e))?;
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR UPDATE")
                .bind(id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_err(context, e))?;
        if locked.is_none() {
            return Err(StoreError::NotFound("user".to_owned()));
        }
        sqlx::query(
            "DELETE FROM user_roles WHERE user_id = $1 AND origin = $2 AND role_id <> ALL($3::uuid[])",
        )
        .bind(id.as_uuid())
        .bind(origin_label(origin))
        .bind(&wanted)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err(context, e))?;
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id, origin) \
             SELECT $1, r, $2 FROM unnest($3::uuid[]) AS r ON CONFLICT DO NOTHING",
        )
        .bind(id.as_uuid())
        .bind(origin_label(origin))
        .bind(&wanted)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err(context, e))?;
        notify(&mut tx, &InvalidationEvent::User(id.as_uuid())).await?;
        tx.commit().await.map_err(|e| map_err(context, e))
    }

    /// Cascades to the user's tokens and role assignments. Announces the change so other
    /// nodes drop principals cached for the user's tokens.
    async fn delete(&self, id: UserId) -> Result<(), StoreError> {
        update_one(
            &self.pool,
            sqlx::query("DELETE FROM users WHERE id = $1").bind(id.as_uuid()),
            id,
            "delete user",
        )
        .await
    }
}
