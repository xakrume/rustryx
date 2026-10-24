//! `OciTokenService`: turns an authenticated principal and a list of requested scopes into a
//! short lived signed token that carries exactly what RBAC allows.
//!
//! Rules, each with a test named `must_not_*` or stating the property:
//!
//! * every requested action is checked through `RbacChecker::check` for the principal; an
//!   action RBAC denies is left out of the token, a repository the caller cannot see is
//!   answered exactly like one that does not exist (a token without actions), so the token
//!   endpoint cannot be used to discover repositories;
//! * `push` and `delete` are only ever granted on hosted repositories: a group or a proxy is
//!   read only through the registry API, whatever RBAC says;
//! * a failing authorization backend is an error (503), never "no access" and never access;
//! * the lifetime is fixed by configuration (5 minutes), not by the request.

use std::sync::Arc;
use std::time::Duration;

use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action as RbacAction, Principal};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{Clock, Coordinate, RbacChecker, RepositoryRepo, ResourceRef};
use time::OffsetDateTime;
use uuid::Uuid;

use super::claims::{
    AccessGrant, GrantResource, RegistryTokenClaims, TokenError, TokenIssuer, TokenVerifier,
};
use crate::parse::scope::{Action, Resource, Scope, ScopeError, parse_scopes};

/// Settings of the token service.
#[derive(Debug, Clone)]
pub struct TokenServiceConfig {
    /// The `service` the registry announces in its challenge and expects as audience.
    pub service: String,
    /// The `iss` of issued tokens.
    pub issuer: String,
    /// Lifetime of a token. 5 minutes by default.
    pub ttl: Duration,
}

impl Default for TokenServiceConfig {
    fn default() -> Self {
        Self {
            service: "rustryx".to_owned(),
            issuer: "rustryx".to_owned(),
            ttl: Duration::from_secs(300),
        }
    }
}

/// Why a token request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenRequestError {
    /// The `service` parameter names another service.
    #[error("unknown service")]
    UnknownService,
    /// A scope is malformed.
    #[error("invalid scope: {0}")]
    InvalidScope(ScopeError),
    /// Authorization or the repository lookup is unavailable: retry.
    #[error("authorization is unavailable")]
    Unavailable,
    /// The token could not be signed.
    #[error("token could not be issued")]
    Signing(TokenError),
}

/// The parameters of a token request, from the query of a `GET` or the form of a `POST`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenRequest {
    /// `service`.
    pub service: Option<String>,
    /// Every `scope` value, unsplit.
    pub scopes: Vec<String>,
}

impl TokenRequest {
    /// Reads `service` and every `scope` from a query string or an
    /// `application/x-www-form-urlencoded` body.
    #[must_use]
    pub fn from_urlencoded(text: &[u8]) -> Self {
        let mut request = Self::default();
        for (key, value) in url::form_urlencoded::parse(text) {
            match key.as_ref() {
                "service" if request.service.is_none() => {
                    request.service = Some(value.into_owned());
                }
                "scope" => request.scopes.push(value.into_owned()),
                _ => {}
            }
        }
        request
    }
}

/// An issued token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// The signed token.
    pub token: String,
    /// Seconds until it expires.
    pub expires_in: u64,
    /// When it was issued.
    pub issued_at: OffsetDateTime,
    /// The scopes actually granted, space separated, for the `OAuth2` response.
    pub granted_scope: String,
}

/// The token service.
#[derive(Clone)]
pub struct OciTokenService {
    repositories: Arc<dyn RepositoryRepo>,
    rbac: Arc<dyn RbacChecker>,
    issuer: Arc<dyn TokenIssuer>,
    verifier: Arc<dyn TokenVerifier>,
    clock: Arc<dyn Clock>,
    config: TokenServiceConfig,
}

impl OciTokenService {
    /// Builds the service.
    #[must_use]
    pub fn new(
        repositories: Arc<dyn RepositoryRepo>,
        rbac: Arc<dyn RbacChecker>,
        issuer: Arc<dyn TokenIssuer>,
        verifier: Arc<dyn TokenVerifier>,
        clock: Arc<dyn Clock>,
        config: TokenServiceConfig,
    ) -> Self {
        Self {
            repositories,
            rbac,
            issuer,
            verifier,
            clock,
            config,
        }
    }

    /// The service name for challenges.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.config.service
    }

    /// Issues a token for `principal`.
    ///
    /// # Errors
    /// [`TokenRequestError`]: a bad scope or service, an unavailable backend, a signing
    /// failure. Having no access to a requested resource is NOT an error.
    pub async fn issue(
        &self,
        principal: &Principal,
        request: &TokenRequest,
    ) -> Result<IssuedToken, TokenRequestError> {
        if request
            .service
            .as_deref()
            .is_some_and(|service| service != self.config.service)
        {
            return Err(TokenRequestError::UnknownService);
        }
        let scopes = parse_scopes(request.scopes.iter().map(String::as_str))
            .map_err(TokenRequestError::InvalidScope)?;
        let mut access: Vec<AccessGrant> = Vec::new();
        for scope in &scopes {
            let grant = self.grant(principal, scope).await?;
            merge_grant(&mut access, grant);
        }
        let now = self.clock.now();
        let ttl = time::Duration::try_from(self.config.ttl).unwrap_or(time::Duration::minutes(5));
        let claims = RegistryTokenClaims {
            issuer: self.config.issuer.clone(),
            subject: principal.username.clone(),
            subject_id: principal.user_id,
            audience: self.config.service.clone(),
            issued_at: now,
            not_before: now,
            expires_at: now + ttl,
            token_id: Uuid::now_v7().to_string(),
            access,
        };
        let token = self
            .issuer
            .issue(&claims)
            .map_err(TokenRequestError::Signing)?;
        Ok(IssuedToken {
            token,
            expires_in: self.config.ttl.as_secs().max(1),
            issued_at: now,
            granted_scope: granted_scope(&claims.access),
        })
    }

    /// Verifies a bearer token presented on `/v2/*`.
    ///
    /// # Errors
    /// The [`TokenError`] that says why it is not acceptable.
    pub fn verify(&self, token: &str) -> Result<RegistryTokenClaims, TokenError> {
        self.verifier.verify(token, self.clock.now())
    }

    /// What `principal` may have of one scope: the intersection of the requested actions,
    /// what the repository type allows and what RBAC allows.
    async fn grant(
        &self,
        principal: &Principal,
        scope: &Scope,
    ) -> Result<AccessGrant, TokenRequestError> {
        match &scope.resource {
            Resource::Catalog => Ok(AccessGrant {
                resource: GrantResource::Catalog,
                // Anyone may hold the catalog grant: the catalog itself lists only what the
                // caller may browse, whoever asks.
                actions: vec![Action::Pull],
            }),
            Resource::Repository { repository, image } => {
                let name = format!("{repository}/{image}");
                let empty = AccessGrant {
                    resource: GrantResource::Repository(name),
                    actions: Vec::new(),
                };
                let found = self
                    .repositories
                    .get_by_name(repository)
                    .await
                    .map_err(|_| TokenRequestError::Unavailable)?;
                let Some(repo) =
                    found.filter(|r| matches!(r.format, RepoFormat::Docker | RepoFormat::Oci))
                else {
                    return Ok(empty);
                };
                let mut actions = Vec::new();
                for action in &scope.actions {
                    if !repository_type_allows(repo.repo_type, *action) {
                        continue;
                    }
                    if self
                        .rbac_allows(principal, &repo, image.as_str(), *action)
                        .await?
                    {
                        actions.push(*action);
                    }
                }
                Ok(AccessGrant { actions, ..empty })
            }
        }
    }

    async fn rbac_allows(
        &self,
        principal: &Principal,
        repo: &Repository,
        image: &str,
        action: Action,
    ) -> Result<bool, TokenRequestError> {
        let (rbac_action, path) = match action {
            Action::Pull => (RbacAction::Read, format!("{image}/tags/list")),
            Action::Push => (RbacAction::Write, format!("{image}/blobs/uploads")),
            Action::Delete => (RbacAction::Delete, format!("{image}/manifests/latest")),
        };
        let leaf = image.rsplit('/').next().unwrap_or(image);
        let resource = ResourceRef::for_repository(repo, path).with_coordinate(Coordinate::new(
            Some(image.to_owned()),
            Some(leaf.to_owned()),
            None,
        ));
        match self.rbac.check(principal, rbac_action, &resource).await {
            Ok(rustryx_core::ports::Decision::Allow) => Ok(true),
            Ok(rustryx_core::ports::Decision::Deny) => Ok(false),
            Err(AuthError::Overloaded) => Err(TokenRequestError::Unavailable),
            Err(error) => {
                // Group lookup failure, provider outage: not "no access" and never access.
                tracing::warn!(%error, repository = %repo.name, "token scope authorization failed closed");
                Err(TokenRequestError::Unavailable)
            }
        }
    }
}

/// `push` and `delete` exist on hosted repositories only.
const fn repository_type_allows(repo_type: RepoType, action: Action) -> bool {
    match action {
        Action::Pull => true,
        Action::Push | Action::Delete => matches!(repo_type, RepoType::Hosted),
    }
}

/// Unions grants for the same resource, so two scopes for one image become one entry.
fn merge_grant(access: &mut Vec<AccessGrant>, grant: AccessGrant) {
    if let Some(existing) = access.iter_mut().find(|g| g.resource == grant.resource) {
        for action in grant.actions {
            if !existing.actions.contains(&action) {
                existing.actions.push(action);
            }
        }
        existing.actions.sort();
    } else {
        access.push(grant);
    }
}

fn granted_scope(access: &[AccessGrant]) -> String {
    access
        .iter()
        .map(|grant| {
            let actions: Vec<&str> = grant.actions.iter().map(|a| a.as_str()).collect();
            match &grant.resource {
                GrantResource::Repository(name) => {
                    format!("repository:{name}:{}", actions.join(","))
                }
                GrantResource::Catalog => "registry:catalog:*".to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
