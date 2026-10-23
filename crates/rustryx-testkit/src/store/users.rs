//! Users, role assignments, API tokens and failed-login counters.

use std::collections::HashSet;

use async_trait::async_trait;
use rustryx_core::Secret;
use rustryx_core::error::StoreError;
use rustryx_core::identity::{ApiToken, NewUser, RoleOrigin, RoleRef, User, UserSource};
use rustryx_core::ids::{ApiTokenId, RoleId, UserId};
use rustryx_core::ports::{
    InvalidationEvent, LoginAttemptRepo, LoginKey, LoginState, Page, PageRequest, TokenRepo,
    UserRepo,
};
use time::OffsetDateTime;

use super::paging::paginate;
use super::{MemStore, conflict, not_found};

/// Stable sort rank so `roles_of` returns the same order on every call.
const fn origin_rank(origin: RoleOrigin) -> u8 {
    match origin {
        RoleOrigin::Manual => 0,
        RoleOrigin::Oidc => 1,
        RoleOrigin::Ldap => 2,
    }
}

#[async_trait]
impl UserRepo for MemStore {
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError> {
        Ok(self.tables()?.users.get(&id).cloned())
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<User>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .users
            .values()
            .find(|u| u.username == username)
            .cloned())
    }

    async fn find_external(
        &self,
        source: UserSource,
        issuer: Option<&str>,
        subject: Option<&str>,
    ) -> Result<Option<User>, StoreError> {
        // Without a subject there is nothing to match on. Falling back to username or
        // email here would let an identity provider claim someone else's account.
        let Some(subject) = subject else {
            return Ok(None);
        };
        let state = self.tables()?;
        Ok(state
            .users
            .values()
            .find(|u| {
                u.source == source
                    && u.issuer.as_deref() == issuer
                    && u.subject.as_deref() == Some(subject)
            })
            .cloned())
    }

    async fn create(&self, new: &NewUser) -> Result<User, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if state.users.values().any(|u| u.username == new.username) {
            return Err(conflict(format!(
                "username {} already exists",
                new.username
            )));
        }
        let external_taken = new.source != UserSource::Local
            && new.subject.is_some()
            && state.users.values().any(|u| {
                u.source == new.source && u.issuer == new.issuer && u.subject == new.subject
            });
        if external_taken {
            return Err(conflict("external identity already exists"));
        }
        let user = User {
            id: UserId::generate(),
            username: new.username.clone(),
            source: new.source,
            issuer: new.issuer.clone(),
            subject: new.subject.clone(),
            email: new.email.clone(),
            display_name: new.display_name.clone(),
            enabled: true,
            password_hash: new.password_hash.clone(),
            tokens_valid_after: None,
            created_at: now,
        };
        state.users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<User>, StoreError> {
        let rows: Vec<(String, User)> = self
            .tables()?
            .users
            .values()
            .map(|u| (u.username.clone(), u.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn set_password_hash(&self, id: UserId, hash: &str) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let user = state
            .users
            .get_mut(&id)
            .ok_or_else(|| not_found("user", id))?;
        user.password_hash = Some(Secret::new(hash));
        Ok(())
    }

    async fn set_enabled(&self, id: UserId, enabled: bool) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let user = state
            .users
            .get_mut(&id)
            .ok_or_else(|| not_found("user", id))?;
        user.enabled = enabled;
        drop(state);
        self.notify(InvalidationEvent::User(id.as_uuid()));
        Ok(())
    }

    async fn set_tokens_valid_after(
        &self,
        id: UserId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let user = state
            .users
            .get_mut(&id)
            .ok_or_else(|| not_found("user", id))?;
        user.tokens_valid_after = Some(at);
        drop(state);
        self.notify(InvalidationEvent::User(id.as_uuid()));
        Ok(())
    }

    async fn roles_of(&self, id: UserId) -> Result<Vec<(RoleRef, RoleOrigin)>, StoreError> {
        let state = self.tables()?;
        let mut roles: Vec<(RoleRef, RoleOrigin)> = state
            .user_roles
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|(role_id, origin)| {
                let role = state.roles.get(role_id)?;
                Some((
                    RoleRef {
                        id: role.id,
                        name: role.name.clone(),
                    },
                    *origin,
                ))
            })
            .collect();
        roles.sort_by_key(|(role, origin)| (role.name.clone(), origin_rank(*origin)));
        Ok(roles)
    }

    async fn replace_roles_for_origin(
        &self,
        id: UserId,
        origin: RoleOrigin,
        roles: &[RoleId],
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.users.contains_key(&id) {
            return Err(not_found("user", id));
        }
        // Validate everything first so a bad role id cannot leave the user half updated.
        if let Some(missing) = roles.iter().find(|r| !state.roles.contains_key(r)) {
            return Err(not_found("role", missing));
        }
        let mut seen = HashSet::new();
        let assigned = state.user_roles.entry(id).or_default();
        // Only rows of `origin` are replaced: a manual grant must survive an OIDC or LDAP
        // sync that no longer lists the group, and the reverse.
        assigned.retain(|(_, existing)| *existing != origin);
        assigned.extend(
            roles
                .iter()
                .filter(|role| seen.insert(**role))
                .map(|role| (*role, origin)),
        );
        drop(state);
        self.notify(InvalidationEvent::User(id.as_uuid()));
        Ok(())
    }

    async fn delete(&self, id: UserId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.users.remove(&id).is_none() {
            return Err(not_found("user", id));
        }
        state.user_roles.remove(&id);
        state.tokens.retain(|_, token| token.user_id != id);
        drop(state);
        self.notify(InvalidationEvent::User(id.as_uuid()));
        Ok(())
    }
}

#[async_trait]
impl TokenRepo for MemStore {
    async fn create(&self, token: &ApiToken) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let duplicate = state
            .tokens
            .values()
            .any(|t| t.id == token.id || t.token_hash == token.token_hash);
        if duplicate {
            return Err(conflict("api token already exists"));
        }
        state.tokens.insert(token.id, token.clone());
        Ok(())
    }

    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<ApiToken>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .tokens
            .values()
            .find(|t| t.token_hash == token_hash)
            .cloned())
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<ApiToken>, StoreError> {
        let state = self.tables()?;
        let mut tokens: Vec<ApiToken> = state
            .tokens
            .values()
            .filter(|t| t.user_id == user_id)
            .cloned()
            .collect();
        tokens.sort_by_key(|t| (t.created_at, t.id));
        Ok(tokens)
    }

    async fn revoke(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let token = state
            .tokens
            .get_mut(&id)
            .ok_or_else(|| not_found("api token", id))?;
        // Revoking twice keeps the first time: the audit trail says when access ended.
        token.revoked_at.get_or_insert(at);
        let owner = token.user_id;
        drop(state);
        self.notify(InvalidationEvent::User(owner.as_uuid()));
        Ok(())
    }

    async fn touch_last_used(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let token = state
            .tokens
            .get_mut(&id)
            .ok_or_else(|| not_found("api token", id))?;
        token.last_used_at = Some(token.last_used_at.map_or(at, |old| old.max(at)));
        Ok(())
    }
}

#[async_trait]
impl LoginAttemptRepo for MemStore {
    async fn register_failure(
        &self,
        key: &LoginKey,
        at: OffsetDateTime,
    ) -> Result<LoginState, StoreError> {
        let mut state = self.tables()?;
        // Read-modify-write under the one lock stands in for the atomic SQL upsert.
        let entry = state
            .login_attempts
            .entry(key.clone())
            .or_insert(LoginState {
                failures: 0,
                last_failure_at: None,
            });
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failure_at = Some(at);
        Ok(entry.clone())
    }

    async fn state(&self, key: &LoginKey) -> Result<LoginState, StoreError> {
        let state = self.tables()?;
        Ok(state
            .login_attempts
            .get(key)
            .cloned()
            .unwrap_or(LoginState {
                failures: 0,
                last_failure_at: None,
            }))
    }

    async fn reset(&self, key: &LoginKey) -> Result<(), StoreError> {
        self.tables()?.login_attempts.remove(key);
        Ok(())
    }
}
