//! Ports of projects, members, labels and robot tokens.
//!
//! Contract as in [`crate::ports`]: typed errors, one statement or one transaction per method,
//! and therefore cancel safe (a cancelled future leaves the old or the new state, never a
//! mix). Absent targets follow the rules of [`crate::ports::repos`].

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::identity::IdpProvider;
use crate::ids::{
    ComponentId, LabelId, ProjectId, ProjectMemberId, RepositoryId, RobotTokenId, UserId,
};
use crate::label::{Label, LabelScope};
use crate::ports::auth::ProjectVisibilityFilter;
use crate::ports::repos::{Page, PageRequest};
use crate::project::{Project, ProjectMember, ProjectName, ProjectRole};
use crate::robot::RobotToken;

/// A project with the counts a list view shows.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    /// The project.
    pub project: Project,
    /// Repositories attached to it.
    pub repository_count: u32,
    /// Members (users and groups).
    pub member_count: u32,
}

/// What still depends on a project, shown when a delete is refused.
#[derive(Debug, Clone)]
pub struct ProjectDependents {
    /// Attached repositories.
    pub repositories: Vec<RepositoryId>,
    /// Robot tokens of the project.
    pub robots: u32,
    /// Webhooks of the project.
    pub webhooks: u32,
}

/// Projects.
#[async_trait]
pub trait ProjectRepo: Send + Sync {
    /// Inserts the project and, when `first_admin` is given, that user as `project_admin` in
    /// the same transaction. `Conflict` when the name exists (names are lowercase, so the
    /// check is exact).
    async fn create(
        &self,
        project: &Project,
        first_admin: Option<UserId>,
    ) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: ProjectId) -> Result<Option<Project>, StoreError>;
    /// Finds by name.
    async fn get_by_name(&self, name: &ProjectName) -> Result<Option<Project>, StoreError>;
    /// Keyset by name. The filter comes from `RbacChecker::visible_projects`, so a caller
    /// cannot list what it may not see.
    async fn list_visible(
        &self,
        filter: &ProjectVisibilityFilter,
        query: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<ProjectSummary>, StoreError>;
    /// Replaces description and visibility. `Conflict` on a stale `expected_version`. A
    /// visibility change rewrites `allow_anonymous` of every repository of the project in the
    /// same transaction (trigger) and publishes `InvalidationEvent::Projects`.
    async fn update(&self, project: &Project, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while any repository is attached (no cascade). Robots and members go with
    /// the project.
    async fn delete(&self, id: ProjectId) -> Result<(), StoreError>;
    /// What still depends on the project.
    async fn dependents(&self, id: ProjectId) -> Result<ProjectDependents, StoreError>;
    /// One transaction: sets `repositories.project_id`, removes project scoped label links of
    /// that repository's components that belong to another project, publishes `Projects`.
    /// `Conflict` when it is attached elsewhere.
    async fn attach_repository(
        &self,
        project: ProjectId,
        repository: RepositoryId,
    ) -> Result<(), StoreError>;
    /// Sets `project_id` to NULL and `allow_anonymous` to false in one statement (fail safe: a
    /// detached repository is never public by accident).
    async fn detach_repository(&self, repository: RepositoryId) -> Result<(), StoreError>;
}

/// Members of a project.
#[async_trait]
pub trait ProjectMemberRepo: Send + Sync {
    /// Lists with keyset pagination.
    async fn list(
        &self,
        project: ProjectId,
        page: &PageRequest,
    ) -> Result<Page<ProjectMember>, StoreError>;
    /// Finds by id.
    async fn get(&self, id: ProjectMemberId) -> Result<Option<ProjectMember>, StoreError>;
    /// `Conflict` when the subject already has a row in the project. Publishes `User(id)` or
    /// `Projects`.
    async fn add(&self, member: &ProjectMember) -> Result<(), StoreError>;
    /// `Conflict` when it would remove the last `project_admin` (direct or group), unless
    /// `allow_last_admin` (set only for `nx-admin`).
    async fn set_role(
        &self,
        id: ProjectMemberId,
        role: ProjectRole,
        allow_last_admin: bool,
    ) -> Result<(), StoreError>;
    /// Removes a member. `Conflict` when it would remove the last `project_admin`, unless
    /// `allow_last_admin`.
    async fn remove(&self, id: ProjectMemberId, allow_last_admin: bool) -> Result<(), StoreError>;
}

/// Read side used only by `rustryx-rbac` (never by handlers): what a principal's roles in
/// projects are.
#[async_trait]
pub trait ProjectAccessRepo: Send + Sync {
    /// Direct memberships of a user.
    async fn memberships_of_user(
        &self,
        user: UserId,
    ) -> Result<Vec<(ProjectId, ProjectRole)>, StoreError>;
    /// Every group row of a provider as `(external_group, project, role)`. Bounded:
    /// `StoreError::Invalid` when more than `limit` rows exist, so a runaway table fails closed
    /// instead of loading without bound.
    async fn group_mappings(
        &self,
        provider: IdpProvider,
        limit: u32,
    ) -> Result<Vec<(String, ProjectId, ProjectRole)>, StoreError>;
}

/// Labels and their links to components.
#[async_trait]
pub trait LabelRepo: Send + Sync {
    /// Inserts. `Conflict` on a name clash in the same scope.
    async fn create(&self, label: &Label) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: LabelId) -> Result<Option<Label>, StoreError>;
    /// `System` lists system labels; `Project(p)` lists project labels. The UI shows system
    /// labels first.
    async fn list(&self, scope: LabelScope, page: &PageRequest) -> Result<Page<Label>, StoreError>;
    /// Replaces a label. `Conflict` on a stale `expected_version`.
    async fn update(&self, label: &Label, expected_version: i64) -> Result<(), StoreError>;
    /// Removes the label and its links. Returns how many components lost it (shown in the
    /// delete dialog).
    async fn delete(&self, id: LabelId) -> Result<u64, StoreError>;
    /// Applies a label to a component. A project label on a component of another project is
    /// `Conflict` (the scope guard also holds in SQL). More than 50 labels on one component is
    /// `Invalid`. Idempotent for an existing link.
    async fn apply(
        &self,
        component: ComponentId,
        label: LabelId,
        by: Option<UserId>,
    ) -> Result<(), StoreError>;
    /// Removes a link. Idempotent.
    async fn unapply(&self, component: ComponentId, label: LabelId) -> Result<(), StoreError>;
    /// The labels of a component.
    async fn labels_of(&self, component: ComponentId) -> Result<Vec<Label>, StoreError>;
    /// How many components carry the label.
    async fn usage(&self, label: LabelId) -> Result<u64, StoreError>;
}

/// Filter of the robot listing.
#[derive(Debug, Clone, Default)]
pub struct RobotFilter {
    /// Only the robots of this project.
    pub project: Option<ProjectId>,
    /// Only system robots.
    pub system_only: bool,
    /// Only robots in this state.
    pub state: Option<RobotState>,
}

/// Derived state of a robot, for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotState {
    /// Usable now.
    Active,
    /// Disabled but kept.
    Disabled,
    /// Past its expiry.
    Expired,
    /// Revoked.
    Revoked,
}

/// Robot tokens. A separate table from personal tokens: an old pod ignores an unknown table
/// cleanly, and deleting a creator must not delete robots.
#[async_trait]
pub trait RobotTokenRepo: Send + Sync {
    /// Inserts. `Conflict` on a duplicate name or hash.
    async fn create(&self, token: &RobotToken) -> Result<(), StoreError>;
    /// Indexed lookup by SHA-256 hex (the authenticator's second lookup, after
    /// `TokenRepo::find_by_hash` missed).
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<RobotToken>, StoreError>;
    /// Finds by id.
    async fn get(&self, id: RobotTokenId) -> Result<Option<RobotToken>, StoreError>;
    /// Lists with keyset pagination.
    async fn list(
        &self,
        filter: &RobotFilter,
        page: &PageRequest,
    ) -> Result<Page<RobotToken>, StoreError>;
    /// Replaces hash, prefix and `rotated_at`. A disabled robot stays disabled. The old secret
    /// stops working at once, because only one hash is stored. `Conflict` on a stale version.
    async fn rotate(
        &self,
        id: RobotTokenId,
        new_hash: &str,
        new_prefix: &str,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// Disables or enables. `Conflict` on a stale version.
    async fn set_disabled(
        &self,
        id: RobotTokenId,
        disabled: bool,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// Marks revoked.
    async fn revoke(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Deletes the record.
    async fn delete(&self, id: RobotTokenId) -> Result<(), StoreError>;
    /// Coalesced by the caller to once a minute per token per node.
    async fn touch_last_used(&self, id: RobotTokenId, at: OffsetDateTime)
    -> Result<(), StoreError>;
    /// Deletes robots that expired or were revoked before `before`. Returns the count.
    async fn purge_expired(&self, before: OffsetDateTime) -> Result<u64, StoreError>;
    /// Robots that expire within `within` of `now` and have no expiry notice yet, for
    /// `token_expiry_notice`.
    async fn list_expiring(
        &self,
        within: Duration,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<RobotToken>, StoreError>;
    /// Marks the expiry notice as sent.
    async fn mark_expiry_notified(
        &self,
        id: RobotTokenId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
}
