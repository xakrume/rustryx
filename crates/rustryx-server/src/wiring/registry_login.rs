//! Turns a verified registry bearer into a principal for `/v2`.
//!
//! The token can only narrow what the account may do. Repository grants become `repo:` scopes
//! (pull is read and browse, push is write, delete is delete). An image path inside the grant
//! is checked again by the `/v2` handler, because a scope string cannot name an image. A token
//! with no repository grant carries one malformed scope on purpose: an empty scope list means
//! no restriction.

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_api::RegistryBearerTokens;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, Principal};
use rustryx_core::ports::UserRepo;
use rustryx_format_oci::{Action, GrantResource, OciTokenService, RegistryTokenClaims, TokenError};

/// Verifies registry bearers and loads the account they name.
pub struct RegistryLogin {
    tokens: OciTokenService,
    users: Arc<dyn UserRepo>,
}

impl RegistryLogin {
    /// Builds the acceptor over the token service and the account store.
    #[must_use]
    pub fn new(tokens: OciTokenService, users: Arc<dyn UserRepo>) -> Self {
        Self { tokens, users }
    }

    async fn principal(&self, claims: RegistryTokenClaims) -> Result<Principal, AuthError> {
        let scopes = repo_scopes(&claims);
        if claims.subject == "anonymous" && claims.subject_id.is_none() {
            let mut principal = Principal::anonymous();
            principal.token_scopes = Some(scopes);
            principal.expires_at = Some(claims.expires_at);
            return Ok(principal);
        }
        let Some(user_id) = claims.subject_id else {
            return Err(AuthError::InvalidToken("registry token has no account"));
        };
        let user = self
            .users
            .get(user_id)
            .await
            .map_err(|error| AuthError::Backend {
                context: "read account",
                source: Box::new(error),
            })?
            .ok_or(AuthError::InvalidToken("registry token account is gone"))?;
        if !user.enabled {
            return Err(AuthError::AccountDisabled);
        }
        if user
            .tokens_valid_after
            .is_some_and(|at| claims.issued_at < at)
        {
            return Err(AuthError::TokenRevoked);
        }
        let roles = self
            .users
            .roles_of(user.id)
            .await
            .map_err(|error| AuthError::Backend {
                context: "read account roles",
                source: Box::new(error),
            })?;
        let mut principal = Principal::new(user.username, user.source, AuthMethod::ApiToken);
        principal.user_id = Some(user.id);
        principal.issuer = user.issuer;
        principal.subject = user.subject;
        principal.email = user.email;
        principal.display_name = user.display_name;
        principal.roles = roles.into_iter().map(|(role, _)| role).collect();
        principal.token_scopes = Some(scopes);
        principal.expires_at = Some(claims.expires_at);
        Ok(principal)
    }
}

#[async_trait]
impl RegistryBearerTokens for RegistryLogin {
    async fn accept(&self, token: &str) -> Result<Principal, AuthError> {
        let claims = self.tokens.verify(token).map_err(token_error)?;
        self.principal(claims).await
    }
}

fn token_error(error: TokenError) -> AuthError {
    match error {
        TokenError::Expired => AuthError::TokenExpired,
        TokenError::NotYetValid => AuthError::InvalidToken("registry token is not valid yet"),
        _ => AuthError::InvalidToken("registry token was rejected"),
    }
}

/// `repo:<repository>:<action>` for every repository grant. Pull also grants browse.
fn repo_scopes(claims: &RegistryTokenClaims) -> Vec<String> {
    let mut scopes = Vec::new();
    for grant in &claims.access {
        let GrantResource::Repository(name) = &grant.resource else {
            continue;
        };
        let Some(repository) = name.split('/').next().filter(|part| !part.is_empty()) else {
            continue;
        };
        for action in &grant.actions {
            let label = match action {
                Action::Pull => "read",
                Action::Push => "write",
                Action::Delete => "delete",
            };
            scopes.push(format!("repo:{repository}:{label}"));
            if *action == Action::Pull {
                scopes.push(format!("repo:{repository}:browse"));
            }
        }
    }
    if scopes.is_empty() {
        scopes.push("oci:none".to_owned());
    }
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_format_oci::AccessGrant;

    fn claims(access: Vec<AccessGrant>) -> RegistryTokenClaims {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        RegistryTokenClaims {
            issuer: "rustryx-registry".into(),
            subject: "anonymous".into(),
            subject_id: None,
            audience: "rustryx".into(),
            issued_at: now,
            not_before: now,
            expires_at: now,
            token_id: "t".into(),
            access,
        }
    }

    #[test]
    fn a_pull_grant_is_read_and_browse_on_the_repository_only() {
        let scopes = repo_scopes(&claims(vec![AccessGrant {
            resource: GrantResource::Repository("library/alpine".into()),
            actions: vec![Action::Pull],
        }]));
        assert_eq!(
            scopes,
            vec![
                "repo:library:read".to_owned(),
                "repo:library:browse".to_owned()
            ]
        );
    }

    #[test]
    fn no_repository_grant_does_not_become_an_empty_scope_list() {
        let scopes = repo_scopes(&claims(Vec::new()));
        assert_eq!(scopes, vec!["oci:none".to_owned()]);
    }
}
