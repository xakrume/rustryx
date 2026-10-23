//! The authorizer.
//!
//! This file holds the content decision (`check`, `can_see_repository`, group filtering).
//! The questions that are not about one piece of content (administrator, application areas,
//! projects, delegation) are in [`management`].

mod management;

use std::borrow::{Borrow, Cow};
use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action, AppArea, AuthMethod, Principal, Privilege, PrivilegeGrant};
use rustryx_core::ids::{ContentSelectorId, ProjectId};
use rustryx_core::model::{RepoFormat, Repository};
use rustryx_core::ports::auth::{
    Coordinate, Decision, ProjectVisibilityFilter, RbacChecker, ResourceRef,
};
use rustryx_core::ports::{InvalidationBus, InvalidationEvent, RoleRepo, SelectorRepo};
use rustryx_core::project::ProjectPermission;
use tokio_util::sync::CancellationToken;

use crate::anonymous::AnonymousSwitch;
use crate::config::RbacConfig;
use crate::invalidation::{InvalidationListener, spawn_listener};
use crate::metrics::{self, DecisionKind};
use crate::path::selector_path;
use crate::scope::TokenScopes;
use crate::selector::{RestrictedEngine, SelectorCompileError, SelectorEngine, SelectorInput};
use crate::state::{Builtin, State};

/// Every action, for "can the principal do anything here" questions.
const ALL_ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

/// What a request touches, borrowed so that group filtering does not clone the
/// path once per member.
#[derive(Debug, Clone, Copy)]
struct Target<'a> {
    repository: &'a str,
    format: RepoFormat,
    path: &'a str,
    coordinate: Option<&'a Coordinate>,
    /// The repository's own anonymous flag. An anonymous caller needs it AND the global switch.
    allow_anonymous: bool,
}

impl<'a> From<&'a ResourceRef> for Target<'a> {
    fn from(resource: &'a ResourceRef) -> Self {
        Self {
            repository: &resource.repository,
            format: resource.format,
            path: &resource.path,
            coordinate: resource.coordinate.as_ref(),
            allow_anonymous: resource.allow_anonymous,
        }
    }
}

/// The principal as authorization sees it for one request.
struct Subject {
    anonymous: bool,
    /// Holds the role named `nx-admin` in the store. Admins skip the privilege
    /// lookup: they may do everything.
    admin: bool,
    privileges: Arc<[Privilege]>,
    scopes: TokenScopes,
}

fn repository_matches(grant_repository: &str, repository: &str) -> bool {
    grant_repository == "*" || grant_repository == repository
}

/// A `RepositoryView` grant covers the repository: the name matches (`*` or exact) and the
/// format restriction, if any, equals the repository's format.
fn view_covers(
    grant_format: Option<RepoFormat>,
    grant_repository: &str,
    repository: &str,
    format: RepoFormat,
) -> bool {
    repository_matches(grant_repository, repository) && grant_format.is_none_or(|f| f == format)
}

/// A robot or scan token. Its rights come from its scopes alone (design 3.e rule 4), so the
/// roles a principal of this kind carries are never consulted: it holds no role, no
/// `nx-admin` and no application privilege.
fn is_robot(principal: &Principal) -> bool {
    matches!(
        principal.auth_method,
        AuthMethod::RobotToken | AuthMethod::ScanToken
    )
}

/// Decides whether a principal may perform an action on content.
///
/// Implements [`RbacChecker`] and adds the helpers the HTTP and group layers
/// need. Cheap to clone: clones share the caches.
///
/// # Decision rules for content (`check`)
///
/// 1. An anonymous caller is refused unless the repository allows it
///    (`ResourceRef::allow_anonymous`) **and** the global switch is on
///    ([`Rbac::anonymous_switch`]); `Write` and `Delete` are refused outright. All of this
///    happens before any store access.
/// 2. A token scope list can only narrow the result.
/// 3. The role named `nx-admin` in the store allows everything. The principal's
///    own role names are not trusted, only role ids are matched against the id
///    that the store reports for `nx-admin`.
/// 4. Otherwise the effective privileges of the principal's roles decide: one
///    privilege that lists the action and whose grant matches is enough. There
///    are no deny rules. An anonymous principal is checked as the built-in role
///    `nx-anonymous`, whatever roles it carries, and an `Admin` grant is ignored for it.
///    A robot or scan token holds no role, so it gets nothing here (its scope based
///    rule arrives with the robot tokens of wave 2 stream B).
/// 5. Anything uncertain is a deny: selector that does not compile or cannot be
///    evaluated, selector that no longer exists, a path that is not canonical.
///    A store failure is returned as `Err(AuthError::Backend)`, never as allow;
///    callers must treat `Err` as a refusal.
///
/// The other questions (`is_admin`, `check_app`, `check_project`, `holds_whole_repository`,
/// `visible_projects`) are answered by the same type with the rules in `docs/rbac.md`; each
/// one fails closed.
pub struct Rbac<E: SelectorEngine = RestrictedEngine> {
    state: Arc<State<E>>,
}

impl<E: SelectorEngine> Clone for Rbac<E> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<E: SelectorEngine> std::fmt::Debug for Rbac<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rbac").finish_non_exhaustive()
    }
}

impl Rbac<RestrictedEngine> {
    /// An authorizer with the shipped [`RestrictedEngine`] at its default limits.
    #[must_use]
    pub fn new(
        roles: Arc<dyn RoleRepo>,
        selectors: Arc<dyn SelectorRepo>,
        config: &RbacConfig,
    ) -> Self {
        Self::with_engine(roles, selectors, RestrictedEngine::default(), config)
    }
}

impl<E: SelectorEngine> Rbac<E> {
    /// An authorizer with a specific selector engine, for example a
    /// [`RestrictedEngine`] with custom limits.
    #[must_use]
    pub fn with_engine(
        roles: Arc<dyn RoleRepo>,
        selectors: Arc<dyn SelectorRepo>,
        engine: E,
        config: &RbacConfig,
    ) -> Self {
        Self {
            state: Arc::new(State::new(roles, selectors, engine, config)),
        }
    }

    /// Starts the task that applies cache invalidation messages from `bus`.
    ///
    /// The subscription is taken before this returns, so a message published
    /// right after cannot be missed. The task ends when `cancel` fires or the
    /// bus closes the stream; keep the returned handle and await
    /// [`InvalidationListener::join`] during shutdown. Without a running
    /// listener the caches still expire by TTL.
    #[must_use]
    pub fn spawn_invalidation_listener(
        &self,
        bus: &dyn InvalidationBus,
        cancel: CancellationToken,
    ) -> InvalidationListener {
        spawn_listener(Arc::clone(&self.state), bus.subscribe(), cancel)
    }

    /// Applies an invalidation event to this node's caches. The listener calls
    /// it for bus messages; call it directly only when the bus is not involved.
    pub fn invalidate(&self, event: &InvalidationEvent) {
        self.state.apply(event);
    }

    /// The handle of the global anonymous switch. Clones of it control this authorizer, so the
    /// settings layer can keep one and flip it without rebuilding anything.
    #[must_use]
    pub fn anonymous_switch(&self) -> AnonymousSwitch {
        self.state.anonymous.clone()
    }

    /// Compiles a selector expression with this authorizer's engine. Call it
    /// when a selector is created or edited so that the administrator gets the
    /// error immediately (SEC-18).
    pub fn validate_selector(&self, expression: &str) -> Result<(), SelectorCompileError> {
        self.state.engine.compile(expression).map(|_| ())
    }

    /// Filters the members of a group repository to those the principal may
    /// use for `action` on `path`.
    ///
    /// Authorization is per member (SEC-13): the caller first checks the group
    /// itself with [`RbacChecker::check`], then asks the group pipeline to
    /// serve only from the members returned here, so a user without rights on a
    /// member never receives its data through the group. Members that are
    /// refused are left out silently, which does not reveal that they exist.
    /// The order of `members` is kept.
    ///
    /// The members are repositories, not names, because the decision needs what a
    /// [`ResourceRef`] would carry for each one: its own format and its own anonymous flag.
    /// An anonymous caller therefore never receives a member that has not opted in, even when
    /// the group has. `Arc<Repository>` and `&Repository` slices work as well.
    ///
    /// A store failure is an error and returns no members at all.
    pub async fn filter_visible_members<'m, R: Borrow<Repository>>(
        &self,
        principal: &Principal,
        action: Action,
        members: &'m [R],
        path: &str,
        coordinate: Option<&Coordinate>,
    ) -> Result<Vec<&'m R>, AuthError> {
        let subject = self.subject(principal).await?;
        let mut visible = Vec::with_capacity(members.len());
        for member in members {
            let repository: &Repository = member.borrow();
            let target = Target {
                repository: repository.name.as_str(),
                format: repository.format,
                path,
                coordinate,
                allow_anonymous: repository.allow_anonymous,
            };
            let decision = metrics::decision(
                DecisionKind::Content,
                self.decide(&subject, action, target).await,
            )?;
            if decision == Decision::Allow {
                visible.push(member);
            }
        }
        Ok(visible)
    }

    async fn holds_admin_role(&self, principal: &Principal) -> Result<bool, AuthError> {
        if is_robot(principal) {
            return Ok(false);
        }
        let admin = self.state.builtin_role(Builtin::Admin).await?;
        Ok(admin.is_some_and(|id| principal.roles.iter().any(|r| r.id == id)))
    }

    async fn subject(&self, principal: &Principal) -> Result<Subject, AuthError> {
        let scopes = TokenScopes::from_principal(principal);
        if principal.is_anonymous() {
            // The roles an anonymous principal carries are ignored: `nx-anonymous`
            // is the only role it can ever have.
            let privileges = match self.state.builtin_role(Builtin::Anonymous).await? {
                Some(id) => self.state.privileges(None, vec![id]).await?,
                None => Arc::from(Vec::new()),
            };
            return Ok(Subject {
                anonymous: true,
                admin: false,
                privileges,
                scopes,
            });
        }
        if is_robot(principal) {
            // Same reasoning as for the anonymous principal: whatever role references the
            // principal carries are not evidence, so a robot has none.
            return Ok(Subject {
                anonymous: false,
                admin: false,
                privileges: Arc::from(Vec::new()),
                scopes,
            });
        }
        let admin = self.holds_admin_role(principal).await?;
        let privileges = if admin {
            Arc::from(Vec::new())
        } else {
            let mut role_ids: Vec<_> = principal.roles.iter().map(|r| r.id).collect();
            role_ids.sort_unstable();
            role_ids.dedup();
            self.state.privileges(principal.user_id, role_ids).await?
        };
        Ok(Subject {
            anonymous: false,
            admin,
            privileges,
            scopes,
        })
    }

    /// Denials that need no I/O: an anonymous caller without both anonymous gates or asking to
    /// write, and actions outside the token scopes.
    fn statically_denied(
        &self,
        anonymous: bool,
        scopes: &TokenScopes,
        action: Action,
        target: &Target<'_>,
    ) -> bool {
        // The two anonymous gates live here, in the one function every content decision goes
        // through, so a call site cannot forget either of them (core gap 13).
        let anonymous_refused = anonymous
            && (matches!(action, Action::Write | Action::Delete)
                || !target.allow_anonymous
                || !self.state.anonymous.is_on());
        anonymous_refused || !scopes.allows(action, target.repository)
    }

    async fn decide(
        &self,
        subject: &Subject,
        action: Action,
        target: Target<'_>,
    ) -> Result<Decision, AuthError> {
        if self.statically_denied(subject.anonymous, &subject.scopes, action, &target) {
            return Ok(Decision::Deny);
        }
        if subject.admin {
            return Ok(Decision::Allow);
        }
        let mut canonical_path: Option<Option<Cow<'_, str>>> = None;
        for privilege in subject
            .privileges
            .iter()
            .filter(|p| p.actions.contains(&action))
        {
            let allowed = match &privilege.grant {
                // An anonymous caller never gets administrator rights, even if
                // `nx-anonymous` was misconfigured with the Admin privilege.
                PrivilegeGrant::Admin => !subject.anonymous,
                PrivilegeGrant::RepositoryView { format, repository } => {
                    view_covers(*format, repository, target.repository, target.format)
                }
                PrivilegeGrant::RepositoryContentSelector {
                    repository,
                    selector_id,
                } if repository_matches(repository, target.repository) => {
                    let path = canonical_path
                        .get_or_insert_with(|| selector_path(target.path))
                        .as_deref();
                    self.selector_matches(*selector_id, target, path).await?
                }
                // A selector grant for another repository, `PrivilegeGrant::Application` (it
                // administers an area and never grants content, design 3.e rule 5) and a grant
                // this build does not know cannot be evaluated: all allow nothing here. An
                // unknown value is never an allow.
                _ => false,
            };
            if allowed {
                return Ok(Decision::Allow);
            }
        }
        Ok(Decision::Deny)
    }

    /// Evaluates a selector for a target. A missing or broken selector, a
    /// non-canonical path and an evaluation error all mean "no match"; only a
    /// store failure is an error.
    async fn selector_matches(
        &self,
        id: ContentSelectorId,
        target: Target<'_>,
        path: Option<&str>,
    ) -> Result<bool, AuthError> {
        let Some(path) = path else {
            tracing::debug!(selector_id = %id, "path is not canonical, selector privilege does not apply");
            return Ok(false);
        };
        let Some(selector) = self.state.selector(id).await? else {
            tracing::debug!(selector_id = %id, "privilege refers to a selector that no longer exists");
            return Ok(false);
        };
        let Some(program) = selector.program.as_deref() else {
            return Ok(false);
        };
        let input = SelectorInput {
            format: target.format,
            path,
            repository: target.repository,
            coordinate: target.coordinate,
        };
        match self.state.engine.evaluate(program, &input) {
            Ok(matched) => Ok(matched),
            Err(error) => {
                tracing::debug!(selector_id = %id, %error, "selector could not be evaluated, no match");
                Ok(false)
            }
        }
    }

    async fn is_visible(
        &self,
        subject: &Subject,
        repository: &str,
        format: RepoFormat,
    ) -> Result<bool, AuthError> {
        // An anonymous caller sees a repository only through what it may read. (The global
        // switch was checked by the caller before the subject was loaded.)
        let counts = |action: Action| {
            (!subject.anonymous || matches!(action, Action::Browse | Action::Read))
                && subject.scopes.allows(action, repository)
        };
        if subject.admin {
            return Ok(ALL_ACTIONS.into_iter().any(counts));
        }
        for privilege in subject.privileges.iter() {
            if !privilege.actions.iter().any(|a| counts(*a)) {
                continue;
            }
            let applies = match &privilege.grant {
                PrivilegeGrant::Admin => !subject.anonymous,
                PrivilegeGrant::RepositoryView {
                    format: grant_format,
                    repository: grant_repository,
                } => view_covers(*grant_format, grant_repository, repository, format),
                // A selector that is gone or broken matches nothing, so it
                // cannot make the repository visible. Which paths it matches is
                // not evaluated: visibility is about the repository.
                PrivilegeGrant::RepositoryContentSelector {
                    repository: grant_repository,
                    selector_id,
                } => {
                    repository_matches(grant_repository, repository)
                        && self
                            .state
                            .selector(*selector_id)
                            .await?
                            .is_some_and(|s| s.program.is_some())
                }
                // Application grants name no repository and an unknown grant cannot be
                // evaluated: neither makes a repository visible.
                _ => false,
            };
            if applies {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[async_trait]
impl<E: SelectorEngine> RbacChecker for Rbac<E> {
    /// Cancel safe: the only awaits are cache loads and store reads, and an
    /// abandoned load leaves no partial state (a cache entry appears only after
    /// a load completed).
    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Decision, AuthError> {
        let target = Target::from(resource);
        let scopes = TokenScopes::from_principal(principal);
        if self.statically_denied(principal.is_anonymous(), &scopes, action, &target) {
            return metrics::decision(DecisionKind::Content, Ok(Decision::Deny));
        }
        let outcome = match self.subject(principal).await {
            Ok(subject) => self.decide(&subject, action, target).await,
            Err(error) => Err(error),
        };
        metrics::decision(DecisionKind::Content, outcome)
    }

    /// Cancel safe, see [`RbacChecker::check`]. True when some privilege that
    /// applies to the repository allows any action (any of `browse` and `read`
    /// for anonymous callers) within the token scopes.
    ///
    /// An anonymous caller sees nothing while the global switch is off. The repository's own
    /// anonymous flag cannot be seen from here (the signature carries a name and a format),
    /// so a caller that answers 404 for a repository it knows is closed to anonymous callers
    /// must apply that flag itself.
    async fn can_see_repository(
        &self,
        principal: &Principal,
        repository: &str,
        format: RepoFormat,
    ) -> Result<bool, AuthError> {
        if principal.is_anonymous() && !self.state.anonymous.is_on() {
            return Ok(false);
        }
        let subject = self.subject(principal).await?;
        self.is_visible(&subject, repository, format).await
    }

    /// Cancel safe. True when the principal is not anonymous, holds the role that the store
    /// knows as `nx-admin` (matched by role id, ADR-RBAC-4), and its token, if any, carries no
    /// scope restriction. A token restricted to some repositories or actions is never an
    /// administrator, and neither is a robot or scan token.
    async fn is_admin(&self, principal: &Principal) -> Result<bool, AuthError> {
        let outcome = self.admin(principal).await.map(|admin| {
            if admin {
                Decision::Allow
            } else {
                Decision::Deny
            }
        });
        metrics::decision(DecisionKind::Admin, outcome).map(|decision| decision == Decision::Allow)
    }

    /// Cancel safe. Allowed for `nx-admin` and for a privilege with `Application { area }` that
    /// lists the action, both only for an authenticated principal with an unrestricted token.
    /// See `docs/rbac.md`, "Application areas".
    async fn check_app(
        &self,
        principal: &Principal,
        area: AppArea,
        action: Action,
    ) -> Result<Decision, AuthError> {
        metrics::decision(
            DecisionKind::App,
            self.app_decision(principal, area, action).await,
        )
    }

    /// Cancel safe. Today only `nx-admin` with an unrestricted token passes; the membership
    /// based rules arrive with wave 2 stream B. See `docs/rbac.md`, "Projects".
    async fn check_project(
        &self,
        principal: &Principal,
        project: ProjectId,
        permission: ProjectPermission,
    ) -> Result<Decision, AuthError> {
        metrics::decision(
            DecisionKind::Project,
            self.project_decision(principal, project, permission).await,
        )
    }

    /// Cancel safe. True when `nx-admin`, an `Admin` privilege or a `RepositoryView` privilege
    /// covers the whole repository for `action` within the token scopes. A content selector
    /// never counts: a selector only privilege cannot be handed to a robot.
    async fn holds_whole_repository(
        &self,
        principal: &Principal,
        repository: &Repository,
        action: Action,
    ) -> Result<bool, AuthError> {
        self.whole_repository(principal, repository, action).await
    }

    /// Cancel safe. `All` for an administrator, `Member { .. }` for a user with an unrestricted
    /// token, `PublicOnly` for everyone else the filter can describe. See `docs/rbac.md`,
    /// "Projects".
    async fn visible_projects(
        &self,
        principal: &Principal,
    ) -> Result<ProjectVisibilityFilter, AuthError> {
        self.project_filter(principal).await
    }
}
