//! Projects, their members and the read side of project roles.

use std::collections::HashSet;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::identity::IdpProvider;
use rustryx_core::ids::{ProjectId, ProjectMemberId, RepositoryId, UserId};
use rustryx_core::label::LabelScope;
use rustryx_core::model::ProjectRef;
use rustryx_core::ports::{
    InvalidationEvent, Page, PageRequest, ProjectAccessRepo, ProjectDependents, ProjectMemberRepo,
    ProjectRepo, ProjectSummary, ProjectVisibilityFilter,
};
use rustryx_core::project::{
    Project, ProjectMember, ProjectName, ProjectRole, ProjectSubject, ProjectVisibility,
};

use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};

impl State {
    /// Rows whose role is `project_admin`, direct and by group.
    fn admin_count(&self, project: ProjectId) -> usize {
        self.members
            .values()
            .filter(|m| m.project_id == project && m.role == ProjectRole::ProjectAdmin)
            .count()
    }

    /// The last project admin may neither be demoted nor removed, unless the caller is an
    /// `nx-admin` (the trigger of the real schema raises `RX008` and honours
    /// `rustryx.allow_last_admin`).
    fn guard_last_admin(
        &self,
        member: &ProjectMember,
        becomes_admin: bool,
        allow_last_admin: bool,
    ) -> Result<(), StoreError> {
        let loses_admin = member.role == ProjectRole::ProjectAdmin && !becomes_admin;
        if loses_admin && !allow_last_admin && self.admin_count(member.project_id) <= 1 {
            return Err(conflict("a project needs at least one project admin"));
        }
        Ok(())
    }

    fn project_ref(project: &Project) -> ProjectRef {
        ProjectRef {
            id: project.id,
            name: project.name.clone(),
            visibility: project.visibility,
        }
    }

    /// Rewrites the project copy of every attached repository, and its anonymous flag, to the
    /// project's current state (the `projects_visibility_sync` trigger).
    fn resync_repositories(&mut self, project: &Project) {
        for repository in self.repositories.values_mut() {
            if repository
                .project
                .as_ref()
                .is_some_and(|r| r.id == project.id)
            {
                repository.project = Some(Self::project_ref(project));
                repository.allow_anonymous = project.visibility == ProjectVisibility::Public;
            }
        }
    }
}

fn subject_event(subject: &ProjectSubject) -> InvalidationEvent {
    match subject {
        ProjectSubject::User { user_id } => InvalidationEvent::User(user_id.as_uuid()),
        ProjectSubject::Group { .. } => InvalidationEvent::Projects,
    }
}

fn lacks_term(project: &Project, query: Option<&str>) -> bool {
    query.is_some_and(|q| {
        !project
            .name
            .as_str()
            .to_lowercase()
            .contains(&q.trim().to_lowercase())
    })
}

#[async_trait]
impl ProjectRepo for MemStore {
    async fn create(
        &self,
        project: &Project,
        first_admin: Option<UserId>,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let taken = state
            .projects
            .values()
            .any(|p| p.id == project.id || p.name == project.name);
        if taken {
            return Err(conflict(format!("project {} already exists", project.name)));
        }
        state.projects.insert(project.id, project.clone());
        if let Some(user_id) = first_admin {
            // Same transaction as the project, so a project is never born without an admin
            // when one was asked for.
            let mut member = ProjectMember::new(
                ProjectMemberId::generate(),
                project.id,
                ProjectSubject::User { user_id },
                ProjectRole::ProjectAdmin,
                now,
            );
            member.added_by = project.created_by;
            state.members.insert(member.id, member);
        }
        drop(state);
        if let Some(user_id) = first_admin {
            self.notify(InvalidationEvent::User(user_id.as_uuid()));
        }
        Ok(())
    }

    async fn get(&self, id: ProjectId) -> Result<Option<Project>, StoreError> {
        Ok(self.tables()?.projects.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &ProjectName) -> Result<Option<Project>, StoreError> {
        let state = self.tables()?;
        Ok(state.projects.values().find(|p| &p.name == name).cloned())
    }

    async fn list_visible(
        &self,
        filter: &ProjectVisibilityFilter,
        query: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<ProjectSummary>, StoreError> {
        let state = self.tables()?;
        let member_of: HashSet<ProjectId> = match filter {
            ProjectVisibilityFilter::Member {
                user_id,
                provider,
                groups,
            } => state
                .members
                .values()
                .filter(|m| match &m.subject {
                    ProjectSubject::User { user_id: member } => Some(*member) == *user_id,
                    ProjectSubject::Group {
                        provider: member_provider,
                        external_group,
                    } => Some(*member_provider) == *provider && groups.contains(external_group),
                })
                .map(|m| m.project_id)
                .collect(),
            _ => HashSet::new(),
        };
        let rows: Vec<(String, ProjectSummary)> = state
            .projects
            .values()
            .filter(|project| match filter {
                ProjectVisibilityFilter::All => true,
                // A filter this build does not know sees the least: public projects only.
                _ => {
                    project.visibility == ProjectVisibility::Public
                        || member_of.contains(&project.id)
                }
            })
            .filter(|project| !lacks_term(project, query))
            .map(|project| {
                let repository_count = state
                    .repositories
                    .values()
                    .filter(|r| r.project.as_ref().is_some_and(|p| p.id == project.id))
                    .count();
                let member_count = state
                    .members
                    .values()
                    .filter(|m| m.project_id == project.id)
                    .count();
                let summary = ProjectSummary {
                    project: project.clone(),
                    repository_count: u32::try_from(repository_count).unwrap_or(u32::MAX),
                    member_count: u32::try_from(member_count).unwrap_or(u32::MAX),
                };
                (project.name.as_str().to_owned(), summary)
            })
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn update(&self, project: &Project, expected_version: i64) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .projects
            .get(&project.id)
            .ok_or_else(|| not_found("project", project.id))?;
        let version = bump_version(existing.version, expected_version)?;
        // Only description and visibility are editable: the name is the URL and OCI prefix.
        let mut next = existing.clone();
        next.description.clone_from(&project.description);
        next.visibility = project.visibility;
        next.version = version;
        next.updated_at = now;
        state.projects.insert(next.id, next.clone());
        state.resync_repositories(&next);
        drop(state);
        self.notify(InvalidationEvent::Projects);
        Ok(())
    }

    async fn delete(&self, id: ProjectId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.projects.contains_key(&id) {
            return Err(not_found("project", id));
        }
        let attached = state
            .repositories
            .values()
            .any(|r| r.project.as_ref().is_some_and(|p| p.id == id));
        if attached {
            return Err(conflict("project still has repositories"));
        }
        state.projects.remove(&id);
        state.remove_project_rows(id);
        drop(state);
        self.notify(InvalidationEvent::Projects);
        Ok(())
    }

    async fn dependents(&self, id: ProjectId) -> Result<ProjectDependents, StoreError> {
        let state = self.tables()?;
        let mut repositories: Vec<RepositoryId> = state
            .repositories
            .values()
            .filter(|r| r.project.as_ref().is_some_and(|p| p.id == id))
            .map(|r| r.id)
            .collect();
        repositories.sort();
        let robots = state
            .robots
            .values()
            .filter(|robot| robot.project_id == Some(id))
            .count();
        Ok(ProjectDependents {
            repositories,
            robots: u32::try_from(robots).unwrap_or(u32::MAX),
            webhooks: state.webhook_count_of_project(id),
        })
    }

    async fn attach_repository(
        &self,
        project: ProjectId,
        repository: RepositoryId,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let target = state
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| not_found("project", project))?;
        let existing = state
            .repositories
            .get(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        if existing
            .project
            .as_ref()
            .is_some_and(|attached| attached.id != project)
        {
            return Err(conflict("repository is attached to another project"));
        }
        if let Some(stored) = state.repositories.get_mut(&repository) {
            stored.project = Some(State::project_ref(&target));
            stored.allow_anonymous = target.visibility == ProjectVisibility::Public;
        }
        // A project label may only sit on components of its own project, so links to labels
        // of any other project are dropped with the move.
        let components: HashSet<_> = state
            .components
            .values()
            .filter(|c| c.repository_id == repository)
            .map(|c| c.id)
            .collect();
        let foreign: HashSet<_> = state
            .labels
            .values()
            .filter(|label| matches!(label.scope, LabelScope::Project(p) if p != project))
            .map(|label| label.id)
            .collect();
        state.component_labels.retain(|(component, label), _| {
            !(components.contains(component) && foreign.contains(label))
        });
        drop(state);
        self.notify(InvalidationEvent::Projects);
        Ok(())
    }

    async fn detach_repository(&self, repository: RepositoryId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let stored = state
            .repositories
            .get_mut(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        stored.project = None;
        // Fail safe: a repository that leaves its project is never public by accident.
        stored.allow_anonymous = false;
        drop(state);
        self.notify(InvalidationEvent::Projects);
        Ok(())
    }
}

#[async_trait]
impl ProjectMemberRepo for MemStore {
    async fn list(
        &self,
        project: ProjectId,
        page: &PageRequest,
    ) -> Result<Page<ProjectMember>, StoreError> {
        let rows: Vec<(String, ProjectMember)> = self
            .tables()?
            .members
            .values()
            .filter(|m| m.project_id == project)
            .map(|m| (m.id.to_string(), m.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn get(&self, id: ProjectMemberId) -> Result<Option<ProjectMember>, StoreError> {
        Ok(self.tables()?.members.get(&id).cloned())
    }

    async fn add(&self, member: &ProjectMember) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // The CHECK constraint of the table, reported as a conflict like every constraint.
        member
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.projects.contains_key(&member.project_id) {
            return Err(conflict(format!(
                "project {} does not exist",
                member.project_id
            )));
        }
        let taken = state.members.values().any(|m| {
            m.id == member.id || (m.project_id == member.project_id && m.subject == member.subject)
        });
        if taken {
            return Err(conflict("the subject already has a role in the project"));
        }
        state.members.insert(member.id, member.clone());
        drop(state);
        self.notify(subject_event(&member.subject));
        Ok(())
    }

    async fn set_role(
        &self,
        id: ProjectMemberId,
        role: ProjectRole,
        allow_last_admin: bool,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let member = state
            .members
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("project member", id))?;
        state.guard_last_admin(&member, role == ProjectRole::ProjectAdmin, allow_last_admin)?;
        if let Some(stored) = state.members.get_mut(&id) {
            stored.role = role;
        }
        drop(state);
        self.notify(subject_event(&member.subject));
        Ok(())
    }

    async fn remove(&self, id: ProjectMemberId, allow_last_admin: bool) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let member = state
            .members
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("project member", id))?;
        state.guard_last_admin(&member, false, allow_last_admin)?;
        state.members.remove(&id);
        drop(state);
        self.notify(subject_event(&member.subject));
        Ok(())
    }
}

#[async_trait]
impl ProjectAccessRepo for MemStore {
    async fn memberships_of_user(
        &self,
        user: UserId,
    ) -> Result<Vec<(ProjectId, ProjectRole)>, StoreError> {
        let state = self.tables()?;
        let mut rows: Vec<(ProjectId, ProjectRole)> = state
            .members
            .values()
            .filter(|m| m.subject == ProjectSubject::User { user_id: user })
            .map(|m| (m.project_id, m.role))
            .collect();
        rows.sort();
        Ok(rows)
    }

    async fn group_mappings(
        &self,
        provider: IdpProvider,
        limit: u32,
    ) -> Result<Vec<(String, ProjectId, ProjectRole)>, StoreError> {
        let state = self.tables()?;
        let mut rows: Vec<(String, ProjectId, ProjectRole)> = state
            .members
            .values()
            .filter_map(|m| match &m.subject {
                ProjectSubject::Group {
                    provider: row_provider,
                    external_group,
                } if *row_provider == provider => {
                    Some((external_group.clone(), m.project_id, m.role))
                }
                _ => None,
            })
            .collect();
        // Bounded on purpose: a runaway table must fail closed instead of loading without
        // bound into the authorization cache.
        if rows.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(StoreError::Invalid(format!(
                "more than {limit} project group rows"
            )));
        }
        rows.sort();
        Ok(rows)
    }
}
