//! `GET /api/v1/me`: the calling principal.
//!
//! What the roles, the groups and the permission summary say is computed from what THIS request
//! proved (the principal the chain built), not from persisted rows. The summary is a convenience
//! for the UI to show or hide entries; every request is decided on its own.

use axum::Json;
use axum::extract::State;
use axum::http::request::Parts;
use rustryx_core::identity::{
    Action, AppArea, AuthMethod, Principal, PrivilegeGrant, ROLE_NX_ANONYMOUS, RoleOrigin, RoleRef,
    UserSource,
};
use rustryx_core::ids::RoleId;
use rustryx_core::ports::Decision;

use super::AuthApi;
use super::dto::{
    Can, EffectiveRole, MeSystem, MeToken, MeUser, MeView, PermissionSummary,
    RepositoryGrantSummary, RoleRefView, TokenPolicyView,
};
use crate::error::ApiError;

/// The origin of a role when the persisted rows do not say: the account source decides.
fn origin_from_source(source: UserSource) -> RoleOrigin {
    match source {
        UserSource::Oidc => RoleOrigin::Oidc,
        UserSource::Ldap => RoleOrigin::Ldap,
        UserSource::Local => RoleOrigin::Manual,
    }
}

fn effective_roles(principal: &Principal, stored: &[(RoleRef, RoleOrigin)]) -> Vec<EffectiveRole> {
    principal
        .roles
        .iter()
        .map(|role| {
            let origin = stored
                .iter()
                .find(|(known, _)| known.id == role.id)
                .map_or_else(
                    || origin_from_source(principal.source),
                    |(_, origin)| *origin,
                );
            EffectiveRole {
                role: RoleRefView {
                    id: role.id,
                    name: role.name.clone(),
                },
                origin,
                // The principal records the groups that produced its roles as a whole, not
                // which group produced which role, so every role of external origin lists them
                // all. A manual role lists none.
                groups: if origin == RoleOrigin::Manual {
                    Vec::new()
                } else {
                    principal.external_groups.clone()
                },
            }
        })
        .collect()
}

impl AuthApi {
    /// The repository grants of a set of roles, summarized. Administration and application
    /// grants are not repository grants and are left out.
    async fn repository_grants(
        &self,
        roles: &[RoleId],
    ) -> Result<Vec<RepositoryGrantSummary>, ApiError> {
        if roles.is_empty() {
            return Ok(Vec::new());
        }
        let privileges = self.roles.effective_privileges(roles).await?;
        Ok(privileges
            .into_iter()
            .filter_map(|privilege| {
                let (repository, format, selector) = match privilege.grant {
                    PrivilegeGrant::RepositoryView { format, repository } => {
                        (repository, format, false)
                    }
                    PrivilegeGrant::RepositoryContentSelector { repository, .. } => {
                        (repository, None, true)
                    }
                    _ => return None,
                };
                Some(RepositoryGrantSummary {
                    privilege: privilege.name,
                    repository,
                    format,
                    actions: privilege.actions,
                    selector,
                })
            })
            .collect())
    }

    async fn permissions(&self, principal: &Principal) -> Result<PermissionSummary, ApiError> {
        let rbac = self.app.rbac();
        let is_admin = match rbac {
            Some(rbac) if !principal.is_anonymous() => rbac.is_admin(principal).await?,
            _ => false,
        };
        let mut admin_areas: Vec<&'static str> = Vec::new();
        if let Some(rbac) = rbac.filter(|_| !principal.is_anonymous()) {
            for area in AppArea::ALL {
                let allowed = is_admin
                    || rbac.check_app(principal, *area, Action::Read).await? == Decision::Allow;
                if allowed {
                    admin_areas.push(area.as_str());
                }
            }
        }
        let role_ids: Vec<RoleId> = if principal.is_anonymous() {
            // `nx-anonymous` shows only while the global anonymous switch is on.
            if self.app.settings().anonymous_enabled {
                self.roles
                    .get_by_name(ROLE_NX_ANONYMOUS)
                    .await?
                    .map(|role| role.id)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            principal.roles.iter().map(|role| role.id).collect()
        };
        let repository_grants = self.repository_grants(&role_ids).await?;
        let interactive = matches!(
            principal.auth_method,
            AuthMethod::InternalJwt | AuthMethod::OidcBearer
        );
        let can = Can {
            create_repository: is_admin,
            upload: is_admin
                || repository_grants
                    .iter()
                    .any(|grant| grant.actions.contains(&Action::Write)),
            create_personal_token: interactive,
            view_audit: is_admin || admin_areas.contains(&AppArea::Audit.as_str()),
            administer: is_admin,
        };
        Ok(PermissionSummary {
            is_admin,
            admin_areas,
            repository_grants,
            can,
        })
    }

    /// The token a request used, by name (the record holds the name, the principal does not).
    async fn token_of(&self, principal: &Principal) -> Result<Option<MeToken>, ApiError> {
        let (Some(id), Some(user)) = (principal.token_id, principal.user_id) else {
            return Ok(None);
        };
        let tokens = self.tokens.list_for_user(user).await?;
        Ok(tokens
            .into_iter()
            .find(|token| token.id == id)
            .map(|token| MeToken {
                id,
                name: token.name,
                scopes: principal.token_scopes.clone().unwrap_or_default(),
            }))
    }
}

/// `GET /api/v1/me`.
pub(super) async fn me(State(api): State<AuthApi>, parts: Parts) -> Result<Json<MeView>, ApiError> {
    let anonymous = Principal::anonymous();
    let principal = parts.extensions.get::<Principal>().unwrap_or(&anonymous);
    let policy = api.settings.token_policy;
    let system = MeSystem {
        // Read only mode is a UI-M2 setting.
        read_only: false,
        anonymous_enabled: api.app.settings().anonymous_enabled,
        base_url_configured: api.settings.base_url_configured,
    };
    let token_policy = TokenPolicyView {
        default_ttl_days: policy.default_ttl_days,
        max_ttl_days: policy.max_ttl_days,
        require_expiry: policy.require_expiry,
    };
    let permissions = api.permissions(principal).await?;
    if principal.is_anonymous() {
        return Ok(Json(MeView {
            authenticated: false,
            user: None,
            auth_method: AuthMethod::Anonymous,
            roles: Vec::new(),
            groups: Vec::new(),
            groups_confirmed_at: None,
            token: None,
            permissions,
            token_policy,
            system,
        }));
    }
    let Some(user_id) = principal.user_id else {
        return Err(ApiError::internal(
            "an authenticated principal without an account id",
        ));
    };
    // The account row is what the chain just authenticated; a missing one means it was deleted
    // in between, which is a session that ended.
    let Some(user) = api.users.get(user_id).await? else {
        return Err(ApiError::unauthenticated());
    };
    let stored = api.users.roles_of(user_id).await?;
    Ok(Json(MeView {
        authenticated: true,
        user: Some(MeUser {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            email: user.email,
            source: user.source,
            issuer: user.issuer,
            subject: user.subject,
            enabled: user.enabled,
            created_at: user.created_at,
        }),
        auth_method: principal.auth_method,
        roles: effective_roles(principal, &stored),
        groups: principal.external_groups.clone(),
        groups_confirmed_at: principal.groups_confirmed_at,
        token: api.token_of(principal).await?,
        permissions,
        token_policy,
        system,
    }))
}

#[cfg(test)]
mod tests;
