//! Questions that are not about one piece of content: administrator, application areas,
//! projects and delegation to robots.
//!
//! Every method here fails closed and keeps the same contract as the content decision: a
//! store failure is `Err`, never an allow, and anything this build cannot evaluate is a
//! refusal. The `RbacChecker` implementation in the parent module wraps these with the
//! decision metrics.

use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action, AppArea, IdpProvider, Principal, PrivilegeGrant, UserSource};
use rustryx_core::ids::ProjectId;
use rustryx_core::model::Repository;
use rustryx_core::ports::auth::{Decision, ProjectVisibilityFilter};
use rustryx_core::project::ProjectPermission;

use super::{Rbac, is_robot, view_covers};
use crate::scope::TokenScopes;
use crate::selector::SelectorEngine;

/// The provider whose group rows apply to a principal of this source. A local account has no
/// external groups, so it matches no group row at all.
const fn provider_of(source: UserSource) -> Option<IdpProvider> {
    match source {
        UserSource::Oidc => Some(IdpProvider::Oidc),
        UserSource::Ldap => Some(IdpProvider::Ldap),
        UserSource::Local => None,
    }
}

impl<E: SelectorEngine> Rbac<E> {
    /// Administrator: authenticated, holds the role the store knows as `nx-admin` (by id), and
    /// carries no scope restriction. A restricted token is never an administrator because the
    /// scope grammar describes content, and mapping it onto administration would widen it.
    pub(super) async fn admin(&self, principal: &Principal) -> Result<bool, AuthError> {
        if principal.is_anonymous() || !TokenScopes::from_principal(principal).is_unrestricted() {
            return Ok(false);
        }
        // Robots and scan tokens are refused inside `holds_admin_role`.
        self.holds_admin_role(principal).await
    }

    /// Application area privilege (`app.<area>.read` and `.write`).
    ///
    /// Allowed for `nx-admin`, or for a role privilege `Application { area }` that lists the
    /// action. An `Admin` privilege held by another role does NOT pass: it means "every
    /// action on all content", while administration is gated by the role id of `nx-admin`
    /// (ADR-RBAC-4) or by an explicit area grant, so that no second route to administrator
    /// rights exists. Anonymous callers, robots and restricted tokens get nothing, for the
    /// reason given on [`Self::admin`].
    pub(super) async fn app_decision(
        &self,
        principal: &Principal,
        area: AppArea,
        action: Action,
    ) -> Result<Decision, AuthError> {
        if principal.is_anonymous()
            || is_robot(principal)
            || !TokenScopes::from_principal(principal).is_unrestricted()
        {
            return Ok(Decision::Deny);
        }
        let subject = self.subject(principal).await?;
        if subject.admin {
            return Ok(Decision::Allow);
        }
        let granted = subject
            .privileges
            .iter()
            .filter(|privilege| privilege.actions.contains(&action))
            .any(|privilege| {
                matches!(
                    &privilege.grant,
                    PrivilegeGrant::Application { area: granted } if *granted == area
                )
            });
        Ok(if granted {
            Decision::Allow
        } else {
            Decision::Deny
        })
    }

    /// Project level permission.
    ///
    /// Today only `nx-admin` with an unrestricted token passes ("`nx-admin` holds all of them
    /// everywhere", design 3.e). Everything else is refused: the project roles, the public
    /// project rule and the robot `scan` rule need the membership caches of wave 2 stream B
    /// (`ProjectAccessRepo`), and a refusal is the safe answer until they exist. `project`
    /// and `permission` are the inputs those rules will read.
    pub(super) async fn project_decision(
        &self,
        principal: &Principal,
        _project: ProjectId,
        _permission: ProjectPermission,
    ) -> Result<Decision, AuthError> {
        Ok(if self.admin(principal).await? {
            Decision::Allow
        } else {
            Decision::Deny
        })
    }

    /// True when the principal holds `action` on the whole repository, with no selector
    /// restriction, within its token scopes.
    ///
    /// This is the test for handing an action to a robot token: a robot cannot exceed its
    /// creator, and a selector only privilege cannot be delegated because the robot would carry
    /// it without the selector. Anonymous callers and robots hold nothing to delegate.
    pub(super) async fn whole_repository(
        &self,
        principal: &Principal,
        repository: &Repository,
        action: Action,
    ) -> Result<bool, AuthError> {
        let name = repository.name.as_str();
        if principal.is_anonymous()
            || is_robot(principal)
            || !TokenScopes::from_principal(principal).allows(action, name)
        {
            return Ok(false);
        }
        let subject = self.subject(principal).await?;
        if subject.admin {
            return Ok(true);
        }
        Ok(subject
            .privileges
            .iter()
            .filter(|privilege| privilege.actions.contains(&action))
            .any(|privilege| match &privilege.grant {
                PrivilegeGrant::Admin => true,
                PrivilegeGrant::RepositoryView {
                    format,
                    repository: granted,
                } => view_covers(*format, granted, name, repository.format),
                // A content selector is a restriction, so it is not the whole repository, and a
                // grant this build does not know cannot be evaluated.
                _ => false,
            }))
    }

    /// Which projects the principal may see, as the filter that `ProjectRepo::list_visible`
    /// takes.
    ///
    /// - An administrator (unrestricted token) sees all.
    /// - A user with an unrestricted token sees public projects plus the ones where the user id
    ///   or one of the external groups has a row. The groups go through unmodified, because the
    ///   SQL matches them exactly; they are passed only together with the provider they came
    ///   from, and a local account has none.
    /// - A restricted token, a robot and a scan token see public projects only: the filter has
    ///   no smaller answer, and none of them holds a project permission.
    /// - An anonymous caller sees public projects only while anonymous access is on. With the
    ///   switch off the answer is an error, never a list.
    pub(super) async fn project_filter(
        &self,
        principal: &Principal,
    ) -> Result<ProjectVisibilityFilter, AuthError> {
        if principal.is_anonymous() {
            return if self.state.anonymous.is_on() {
                Ok(ProjectVisibilityFilter::PublicOnly)
            } else {
                Err(AuthError::AccessDenied("anonymous access is disabled"))
            };
        }
        if is_robot(principal) || !TokenScopes::from_principal(principal).is_unrestricted() {
            return Ok(ProjectVisibilityFilter::PublicOnly);
        }
        if self.holds_admin_role(principal).await? {
            return Ok(ProjectVisibilityFilter::All);
        }
        let provider = provider_of(principal.source);
        let groups = if provider.is_some() {
            principal.external_groups.clone()
        } else {
            Vec::new()
        };
        Ok(ProjectVisibilityFilter::Member {
            user_id: principal.user_id,
            provider,
            groups,
        })
    }
}
