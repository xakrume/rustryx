//! Authentication, group resolution and authorization ports.
//!
//! `rustryx-auth` answers "who is this" and "which external groups does that
//! identity have". `rustryx-rbac` answers "may this principal do that". The two
//! never mix: authenticators make no authorization decisions.

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::error::AuthError;
use crate::identity::{
    Action, AppArea, AuthContext, AuthFailure, Credentials, IdpProvider, Principal,
};
use crate::ids::{ProjectId, UserId};
use crate::model::{ProjectRef, RepoFormat, Repository};
use crate::project::ProjectPermission;

/// Verifies credentials and produces a principal.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// Cancel safe.
    ///
    /// Returns `Ok(None)` when the credentials are not applicable to this
    /// authenticator (for example an OIDC authenticator given a Basic header),
    /// so the chain can try the next one. Returns `Err` when they are applicable
    /// and wrong, expired, locked out or when a required backend failed.
    /// Roles are not resolved here: the returned principal has `external_groups`
    /// filled where the credential carries them, and `roles` is filled later.
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError>;

    /// Like `authenticate`, but a failure also names the method that refused, so that
    /// `auth_attempts_total` can label it. Cancel safe.
    ///
    /// The default wraps `authenticate` and reports no method. The chain overrides it; every
    /// other implementer inherits the default and needs no change.
    async fn authenticate_detailed(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthFailure> {
        self.authenticate(credentials, context)
            .await
            .map_err(|error| AuthFailure::new(None, error))
    }
}

/// External groups of an authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExternalGroups {
    /// Provider that produced the list.
    pub provider: IdpProvider,
    /// Group ids: full Keycloak group paths or LDAP DNs. Case sensitive, unmodified.
    pub groups: Vec<String>,
    /// When the provider last confirmed the membership. The LDAP five minute window is timed
    /// from the directory's answer, not from receipt.
    pub confirmed_at: OffsetDateTime,
}

impl ExternalGroups {
    /// A group list confirmed at `confirmed_at`.
    #[must_use]
    pub fn new(provider: IdpProvider, groups: Vec<String>, confirmed_at: OffsetDateTime) -> Self {
        Self {
            provider,
            groups,
            confirmed_at,
        }
    }
}

/// One interface over the Keycloak `groups` claim and LDAP group search.
/// The mapping group -> role is shared and lives in `rustryx-auth`
/// (`RoleMapper`, backed by `idp_group_mappings`).
#[async_trait]
pub trait GroupResolver: Send + Sync {
    /// Cancel safe.
    ///
    /// Returns the groups for the principal's current request. A failed or timed
    /// out lookup is an `Err(AuthError::GroupLookupFailed)`, never an empty list:
    /// callers fail closed. A missing or empty OIDC `groups` claim is a valid
    /// empty result and removes roles of origin `oidc` for this request.
    async fn external_groups(&self, principal: &Principal) -> Result<ExternalGroups, AuthError>;
}

/// What is being accessed, as seen by authorization.
///
/// Build it with [`ResourceRef::for_repository`], which copies the anonymous flag and the
/// project from the repository: a caller that forgets either does not compile, and `check`
/// fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceRef {
    /// Repository name.
    pub repository: String,
    /// Repository format.
    pub format: RepoFormat,
    /// Normalized path produced by the format's `classify`.
    pub path: String,
    /// Component coordinate when the format can derive one before reading the body.
    pub coordinate: Option<Coordinate>,
    /// `repositories.allow_anonymous`. An anonymous caller is allowed only when this and the
    /// global anonymous switch are both true; both gates live inside `RbacChecker::check`.
    pub allow_anonymous: bool,
    /// The project of the repository, for project roles.
    pub project: Option<ProjectRef>,
}

impl ResourceRef {
    /// The only constructor in production code: copies name, format, anonymous flag and
    /// project from the repository.
    #[must_use]
    pub fn for_repository(repository: &Repository, path: impl Into<String>) -> Self {
        Self {
            repository: repository.name.as_str().to_owned(),
            format: repository.format,
            path: path.into(),
            coordinate: None,
            allow_anonymous: repository.allow_anonymous,
            project: repository.project.clone(),
        }
    }

    /// Attaches the component coordinate.
    #[must_use]
    pub fn with_coordinate(mut self, coordinate: Coordinate) -> Self {
        self.coordinate = Some(coordinate);
        self
    }
}

/// Format neutral component coordinate exposed to content selectors as `coordinate.*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Coordinate {
    /// Namespace (groupId, scope).
    pub group: Option<String>,
    /// Package name.
    pub name: Option<String>,
    /// Version.
    pub version: Option<String>,
    /// File extension (maven `coordinate.extension`).
    pub extension: Option<String>,
    /// Classifier (maven `coordinate.classifier`).
    pub classifier: Option<String>,
}

impl Coordinate {
    /// A coordinate with group, name and version; extension and classifier are `None`.
    #[must_use]
    pub fn new(group: Option<String>, name: Option<String>, version: Option<String>) -> Self {
        Self {
            group,
            name,
            version,
            extension: None,
            classifier: None,
        }
    }
}

/// Result of an authorization check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allowed.
    Allow,
    /// Not allowed (also when the information needed to decide is missing).
    Deny,
}

/// Answer of `RbacChecker::visible_projects`, passed to `ProjectRepo::list_visible`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectVisibilityFilter {
    /// Administrators see every project.
    All,
    /// Public projects only (anonymous callers, and the default of an implementer that knows no
    /// projects).
    PublicOnly,
    /// Public projects plus the projects in which the caller has a role (user id and external
    /// groups).
    Member {
        /// The caller's account, for direct memberships.
        user_id: Option<UserId>,
        /// The provider of `groups`.
        provider: Option<IdpProvider>,
        /// The caller's external groups, matched exactly.
        groups: Vec<String>,
    },
}

/// Authorization: effective privileges, content selectors, project roles and robot scopes.
///
/// The methods after `can_see_repository` are provided and fail closed: an implementer that
/// knows no projects, no application areas and no robots refuses everything instead of
/// allowing it.
#[async_trait]
pub trait RbacChecker: Send + Sync {
    /// Cancel safe.
    ///
    /// Decision order: anonymous write denied (and anonymous read denied unless
    /// `resource.allow_anonymous` and the global switch are both on), token scopes, `nx-admin`
    /// by role id, robot and scan token scopes, role privileges, project roles, then deny.
    /// There are no deny rules.
    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Decision, AuthError>;

    /// Cancel safe. True when the principal may browse at least something in
    /// the repository. Used to hide repositories (404 instead of 403).
    async fn can_see_repository(
        &self,
        principal: &Principal,
        repository: &str,
        format: RepoFormat,
    ) -> Result<bool, AuthError>;

    /// True when the principal holds `nx-admin` by role id (never by name) and its token, if
    /// any, is unrestricted. Cancel safe. Default `Ok(false)`.
    async fn is_admin(&self, _principal: &Principal) -> Result<bool, AuthError> {
        Ok(false)
    }

    /// Project level permission (members, policies, robots, webhooks, ...). Cancel safe.
    /// Default `Deny`, so an implementer that knows no projects refuses everything.
    async fn check_project(
        &self,
        _principal: &Principal,
        _project: ProjectId,
        _permission: ProjectPermission,
    ) -> Result<Decision, AuthError> {
        Ok(Decision::Deny)
    }

    /// Application area privilege (`app.<area>.read` and `.write`). `nx-admin` always passes.
    /// Cancel safe. Default `Deny`.
    async fn check_app(
        &self,
        _principal: &Principal,
        _area: AppArea,
        _action: Action,
    ) -> Result<Decision, AuthError> {
        Ok(Decision::Deny)
    }

    /// True when the principal holds `action` on the WHOLE repository, with no selector
    /// restriction. This is the condition for handing that action to a robot token (a robot
    /// cannot exceed its creator, and a selector only privilege cannot be delegated). Cancel
    /// safe. Default `Ok(false)`.
    async fn holds_whole_repository(
        &self,
        _principal: &Principal,
        _repository: &Repository,
        _action: Action,
    ) -> Result<bool, AuthError> {
        Ok(false)
    }

    /// Which projects the principal may see: all, or this explicit set plus every public
    /// project. Cancel safe. Default is `PublicOnly`.
    async fn visible_projects(
        &self,
        _principal: &Principal,
    ) -> Result<ProjectVisibilityFilter, AuthError> {
        Ok(ProjectVisibilityFilter::PublicOnly)
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;

    use super::*;
    use crate::identity::{AuthMethod, UserSource};
    use crate::ids::{BlobStoreId, RepositoryId};
    use crate::model::RepoType;
    use crate::names::RepoName;
    use crate::project::{ProjectName, ProjectVisibility};
    use crate::testing::at;

    fn repository() -> Repository {
        Repository::new(
            RepositoryId::generate(),
            RepoName::parse("libs").expect("name"),
            RepoFormat::Maven2,
            RepoType::Hosted,
            BlobStoreId::generate(),
            at(),
        )
    }

    #[test]
    fn resource_ref_copies_the_anonymous_flag_and_the_project_from_the_repository() {
        let mut repo = repository();
        let plain = ResourceRef::for_repository(&repo, "a/b.jar");
        assert_eq!(plain.repository, "libs");
        assert_eq!(plain.format, RepoFormat::Maven2);
        assert_eq!(plain.path, "a/b.jar");
        assert!(!plain.allow_anonymous && plain.project.is_none() && plain.coordinate.is_none());

        repo.allow_anonymous = true;
        repo.project = Some(ProjectRef {
            id: ProjectId::generate(),
            name: ProjectName::parse("payments").expect("name"),
            visibility: ProjectVisibility::Public,
        });
        let public = ResourceRef::for_repository(&repo, String::from("x"));
        assert!(public.allow_anonymous);
        assert_eq!(public.project, repo.project);
    }

    #[test]
    fn must_not_forget_the_anonymous_gate_a_new_repository_is_not_anonymous() {
        let resource = ResourceRef::for_repository(&repository(), "p");
        assert!(
            !resource.allow_anonymous,
            "the default repository is closed to anonymous callers"
        );
    }

    #[test]
    fn coordinate_constructor_and_builder() {
        let coordinate = Coordinate::new(Some("g".into()), Some("n".into()), None);
        assert_eq!(coordinate.group.as_deref(), Some("g"));
        assert!(
            coordinate.version.is_none()
                && coordinate.extension.is_none()
                && coordinate.classifier.is_none()
        );
        let resource =
            ResourceRef::for_repository(&repository(), "p").with_coordinate(coordinate.clone());
        assert_eq!(resource.coordinate, Some(coordinate));
        assert_eq!(Coordinate::default(), Coordinate::new(None, None, None));
    }

    #[test]
    fn external_groups_carry_the_directory_confirmation_time() {
        let groups = ExternalGroups::new(IdpProvider::Ldap, vec!["cn=a".into()], at());
        assert_eq!(groups.confirmed_at, at());
        assert_eq!(groups.provider, IdpProvider::Ldap);
        assert_eq!(groups.groups, ["cn=a"]);
    }

    struct Minimal;

    #[async_trait]
    impl RbacChecker for Minimal {
        async fn check(
            &self,
            _: &Principal,
            _: Action,
            _: &ResourceRef,
        ) -> Result<Decision, AuthError> {
            Ok(Decision::Allow)
        }
        async fn can_see_repository(
            &self,
            _: &Principal,
            _: &str,
            _: RepoFormat,
        ) -> Result<bool, AuthError> {
            Ok(true)
        }
    }

    #[test]
    fn must_not_allow_through_a_provided_rbac_method() {
        let checker: Box<dyn RbacChecker> = Box::new(Minimal);
        let principal = Principal::new("alice", UserSource::Local, AuthMethod::BasicLocal);
        let repo = repository();
        block_on(async {
            assert!(!checker.is_admin(&principal).await.expect("ok"));
            let denied = checker
                .check_project(
                    &principal,
                    ProjectId::generate(),
                    ProjectPermission::ViewProject,
                )
                .await
                .expect("ok");
            assert_eq!(denied, Decision::Deny);
            for area in AppArea::ALL {
                for action in [Action::Read, Action::Write, Action::Delete] {
                    assert_eq!(
                        checker
                            .check_app(&principal, *area, action)
                            .await
                            .expect("ok"),
                        Decision::Deny
                    );
                }
            }
            for action in [Action::Browse, Action::Read, Action::Write, Action::Delete] {
                assert!(
                    !checker
                        .holds_whole_repository(&principal, &repo, action)
                        .await
                        .expect("ok")
                );
            }
            assert_eq!(
                checker.visible_projects(&principal).await.expect("ok"),
                ProjectVisibilityFilter::PublicOnly
            );
        });
    }

    struct Failing;

    #[async_trait]
    impl Authenticator for Failing {
        async fn authenticate(
            &self,
            credentials: &Credentials,
            _: &AuthContext,
        ) -> Result<Option<Principal>, AuthError> {
            match credentials {
                Credentials::Anonymous => Ok(None),
                Credentials::Basic { .. } => Ok(Some(Principal::anonymous())),
                Credentials::Bearer(_) => Err(AuthError::TokenRevoked),
            }
        }
    }

    #[test]
    fn the_detailed_default_wraps_the_plain_result_without_a_method() {
        let authenticator = Failing;
        let context = AuthContext::new();
        block_on(async {
            let failure = authenticator
                .authenticate_detailed(
                    &Credentials::Bearer(crate::secret::Secret::new("t")),
                    &context,
                )
                .await
                .expect_err("refused");
            assert!(failure.method.is_none());
            assert!(matches!(failure.error, AuthError::TokenRevoked));
            assert!(
                authenticator
                    .authenticate_detailed(&Credentials::Anonymous, &context)
                    .await
                    .expect("ok")
                    .is_none()
            );
            let basic = Credentials::Basic {
                username: "u".into(),
                password: crate::secret::Secret::new("p"),
            };
            assert!(
                authenticator
                    .authenticate_detailed(&basic, &context)
                    .await
                    .expect("ok")
                    .is_some()
            );
        });
    }

    #[test]
    fn visibility_filter_member_keeps_groups_unmodified() {
        let filter = ProjectVisibilityFilter::Member {
            user_id: None,
            provider: Some(IdpProvider::Oidc),
            groups: vec![" /Teams/X ".into()],
        };
        assert!(
            matches!(&filter, ProjectVisibilityFilter::Member { groups, .. } if groups[0] == " /Teams/X ")
        );
        assert_ne!(filter, ProjectVisibilityFilter::All);
    }
}
