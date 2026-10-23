//! Projects: an optional container over repositories of any format, with members and roles.
//!
//! The five project roles are a code table ([`ProjectRole::template`]), not database rows, so
//! the matrix is auditable and pinned by a test. `rustryx-rbac` expands the templates; a
//! principal's grants in a project are the union of the templates of all its memberships.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::identity::{Action, IdpProvider};
use crate::ids::{ProjectId, ProjectMemberId, UserId};

/// Names that would clash with a URL space or a UI route. A project name is also an OCI path
/// prefix (`host/<project>/<image>`), so it follows the container naming rules, not the looser
/// `RepoName` ones.
const RESERVED_NAMES: &[&str] = &[
    "api",
    "v2",
    "service",
    "repository",
    "static",
    "assets",
    "metrics",
    "healthz",
    "readyz",
    "login",
    "logout",
    "auth",
    "admin",
    "ui",
    "docs",
    "openapi",
];

/// Shortest project name.
pub const MIN_PROJECT_NAME_LEN: usize = 2;
/// Longest project name.
pub const MAX_PROJECT_NAME_LEN: usize = 64;
/// Longest project description in characters.
pub const MAX_PROJECT_DESCRIPTION_CHARS: usize = 4000;

/// Validated project name.
///
/// 2 to 64 characters, lowercase ASCII letters and digits, with single `.`, `_` or `-` as
/// separators between them (so it neither starts nor ends with a separator and never holds two
/// in a row), not in the reserved list. Uppercase input is rejected rather than lowered: an OCI
/// path component is lowercase and a silent rewrite would make `Payments` and `payments` look
/// like two names in a form while the database holds one. The rule is the one the `projects`
/// table enforces with a CHECK constraint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);

impl ProjectName {
    /// Parses and validates a name.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule is violated.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        if !(MIN_PROJECT_NAME_LEN..=MAX_PROJECT_NAME_LEN).contains(&value.len()) {
            return Err(DomainError::invalid("project name length must be 2 to 64"));
        }
        let shape_ok = value
            .split(['.', '_', '-'])
            .all(|part| !part.is_empty() && part.bytes().all(is_lower_alnum));
        if !shape_ok {
            return Err(DomainError::invalid(
                "project name must consist of lowercase letters and digits separated by single '.', '_' or '-'",
            ));
        }
        if RESERVED_NAMES.contains(&value) {
            return Err(DomainError::invalid("project name is reserved"));
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Who may see a project and read its repositories without being a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectVisibility {
    /// Invisible (404) to non members.
    Private,
    /// Every principal may browse and read; an anonymous caller still needs the global switch.
    Public,
}

/// A project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Project {
    /// Identifier.
    pub id: ProjectId,
    /// Unique lowercase name.
    pub name: ProjectName,
    /// Markdown, at most 4000 characters. Rendered by the UI with a sanitizing renderer.
    pub description: String,
    /// Private or public.
    pub visibility: ProjectVisibility,
    /// Creator, `None` once that account is deleted.
    pub created_by: Option<UserId>,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Project {
    /// A private project with an empty description, no creator and version 1.
    #[must_use]
    pub fn new(id: ProjectId, name: ProjectName, now: OffsetDateTime) -> Self {
        Self {
            id,
            name,
            description: String::new(),
            visibility: ProjectVisibility::Private,
            created_by: None,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks the description limit.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the description exceeds 4000 characters.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.description.chars().count() > MAX_PROJECT_DESCRIPTION_CHARS {
            return Err(DomainError::invalid(
                "project description must be at most 4000 characters",
            ));
        }
        Ok(())
    }
}

/// The five project roles, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRole {
    /// Browse and read, see the project and its quota.
    LimitedGuest,
    /// Limited guest plus findings and the project audit log.
    Guest,
    /// Guest plus push, scans and labels.
    Developer,
    /// Developer plus delete, policies, webhooks and promotion approval.
    Maintainer,
    /// Maintainer plus labels, robots, members and project settings.
    ProjectAdmin,
}

/// Project level actions that are not content actions. `nx-admin` holds all of them everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectPermission {
    /// See the project.
    ViewProject,
    /// See quota usage.
    ViewQuota,
    /// See vulnerability findings in detail.
    ViewFindings,
    /// See the project audit log.
    ViewAudit,
    /// Trigger and stop scans.
    RunScans,
    /// Apply and remove labels on components.
    ApplyLabels,
    /// Create, edit and delete project labels.
    ManageLabels,
    /// Retention, immutability, CVE allowlist, trust, scan and gate settings, quarantine release.
    ManagePolicies,
    /// Project and repository webhooks.
    ManageWebhooks,
    /// Project robot tokens.
    ManageRobots,
    /// Members and their roles.
    ManageMembers,
    /// Description, visibility, deleting the project.
    ManageSettings,
    /// Approve a promotion into a repository of this project.
    ApprovePromotion,
}

impl ProjectPermission {
    /// Every permission, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::ViewProject,
        Self::ViewQuota,
        Self::ViewFindings,
        Self::ViewAudit,
        Self::RunScans,
        Self::ApplyLabels,
        Self::ManageLabels,
        Self::ManagePolicies,
        Self::ManageWebhooks,
        Self::ManageRobots,
        Self::ManageMembers,
        Self::ManageSettings,
        Self::ApprovePromotion,
    ];
}

/// What a role grants. Content actions apply to every repository of the project.
#[derive(Debug, Clone, Copy)]
pub struct ProjectGrantSet {
    /// Content actions on every repository of the project.
    pub content: &'static [Action],
    /// Project level permissions.
    pub project: &'static [ProjectPermission],
}

impl ProjectGrantSet {
    /// True when the set grants the content action.
    #[must_use]
    pub fn allows_content(&self, action: Action) -> bool {
        self.content.contains(&action)
    }

    /// True when the set grants the project permission.
    #[must_use]
    pub fn allows_project(&self, permission: ProjectPermission) -> bool {
        self.project.contains(&permission)
    }
}

impl ProjectRole {
    /// Every role, least privileged first.
    pub const ALL: [Self; 5] = [
        Self::LimitedGuest,
        Self::Guest,
        Self::Developer,
        Self::Maintainer,
        Self::ProjectAdmin,
    ];

    /// The one place the role matrix lives. `project_role_matrix_is_pinned` pins every cell.
    ///
    /// Roles contain no `nx-admin` and no application privilege, so there is nothing to
    /// escalate to. Quota limits, registries, scanners, the system CVE allowlist and global
    /// settings are never project permissions: they stay `nx-admin`.
    #[must_use]
    pub const fn template(self) -> ProjectGrantSet {
        use Action::{Browse, Delete, Read, Write};
        use ProjectPermission::{
            ApplyLabels, ApprovePromotion, ManageLabels, ManageMembers, ManagePolicies,
            ManageRobots, ManageSettings, ManageWebhooks, RunScans, ViewAudit, ViewFindings,
            ViewProject, ViewQuota,
        };
        match self {
            Self::LimitedGuest => ProjectGrantSet {
                content: &[Browse, Read],
                project: &[ViewProject, ViewQuota],
            },
            Self::Guest => ProjectGrantSet {
                content: &[Browse, Read],
                project: &[ViewProject, ViewQuota, ViewFindings, ViewAudit],
            },
            Self::Developer => ProjectGrantSet {
                content: &[Browse, Read, Write],
                project: &[
                    ViewProject,
                    ViewQuota,
                    ViewFindings,
                    ViewAudit,
                    RunScans,
                    ApplyLabels,
                ],
            },
            Self::Maintainer => ProjectGrantSet {
                content: &[Browse, Read, Write, Delete],
                project: &[
                    ViewProject,
                    ViewQuota,
                    ViewFindings,
                    ViewAudit,
                    RunScans,
                    ApplyLabels,
                    ManagePolicies,
                    ManageWebhooks,
                    ApprovePromotion,
                ],
            },
            Self::ProjectAdmin => ProjectGrantSet {
                content: &[Browse, Read, Write, Delete],
                project: &[
                    ViewProject,
                    ViewQuota,
                    ViewFindings,
                    ViewAudit,
                    RunScans,
                    ApplyLabels,
                    ManagePolicies,
                    ManageWebhooks,
                    ApprovePromotion,
                    ManageLabels,
                    ManageRobots,
                    ManageMembers,
                    ManageSettings,
                ],
            },
        }
    }

    /// Canonical snake case label used in JSON and in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LimitedGuest => "limited_guest",
            Self::Guest => "guest",
            Self::Developer => "developer",
            Self::Maintainer => "maintainer",
            Self::ProjectAdmin => "project_admin",
        }
    }

    /// Parses the canonical label.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for any other text.
    pub fn parse(label: &str) -> Result<Self, DomainError> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == label)
            .ok_or_else(|| DomainError::invalid("unknown project role"))
    }
}

/// Who a project role is given to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectSubject {
    /// One user account.
    User {
        /// The account.
        user_id: UserId,
    },
    /// Every member of an external group.
    Group {
        /// Provider the group string belongs to.
        provider: IdpProvider,
        /// Exact, case sensitive, untrimmed: a Keycloak full path or an LDAP DN (README 8.5).
        /// Never a prefix or a pattern.
        external_group: String,
    },
}

/// One membership row: a subject with a role in a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectMember {
    /// Identifier.
    pub id: ProjectMemberId,
    /// The project.
    pub project_id: ProjectId,
    /// The user or group that holds the role.
    pub subject: ProjectSubject,
    /// The role.
    pub role: ProjectRole,
    /// Who added the row.
    pub added_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ProjectMember {
    /// A membership added by nobody in particular (assign `added_by` when known).
    #[must_use]
    pub fn new(
        id: ProjectMemberId,
        project_id: ProjectId,
        subject: ProjectSubject,
        role: ProjectRole,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            project_id,
            subject,
            role,
            added_by: None,
            created_at: now,
        }
    }

    /// Checks the group string of a group subject: non empty and at most 1024 characters, as
    /// the `project_members` constraint requires. The string is never trimmed or normalized.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for an empty or oversized group string.
    pub fn validate(&self) -> Result<(), DomainError> {
        if let ProjectSubject::Group { external_group, .. } = &self.subject
            && (external_group.is_empty() || external_group.chars().count() > 1024)
        {
            return Err(DomainError::invalid("group must be 1 to 1024 characters"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    #[test]
    fn accepts_typical_project_names() {
        for ok in ["payments", "ab", "team-1", "a.b_c-d", "x9", &"a".repeat(64)] {
            assert!(ProjectName::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_unsafe_and_malformed_project_names() {
        let long = "a".repeat(65);
        for bad in [
            "",
            "a",
            "Payments",
            "pay ments",
            "-pay",
            "pay-",
            ".pay",
            "pay..ments",
            "pay--ments",
            "pay_-ments",
            "a/b",
            "é1",
            "pay\u{0}",
            long.as_str(),
        ] {
            assert!(ProjectName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reserved_names_are_refused() {
        for reserved in RESERVED_NAMES {
            assert!(ProjectName::parse(reserved).is_err(), "{reserved}");
        }
        assert!(
            ProjectName::parse("api2").is_ok(),
            "only the whole name is reserved"
        );
        assert!(
            ProjectName::parse("my-api").is_ok(),
            "only the whole name is reserved"
        );
    }

    #[test]
    fn project_name_deserialization_validates() {
        assert!(serde_json::from_str::<ProjectName>("\"payments\"").is_ok());
        assert!(serde_json::from_str::<ProjectName>("\"Payments\"").is_err());
        assert!(serde_json::from_str::<ProjectName>("\"admin\"").is_err());
        assert_eq!(
            serde_json::to_string(&ProjectName::parse("payments").expect("name")).expect("json"),
            "\"payments\""
        );
    }

    #[test]
    fn project_constructor_defaults_and_description_limit() {
        let name = ProjectName::parse("payments").expect("name");
        let mut project = Project::new(ProjectId::generate(), name, at());
        assert_eq!(project.visibility, ProjectVisibility::Private);
        assert_eq!(project.version, 1);
        assert!(project.created_by.is_none());
        assert_eq!(project.created_at, project.updated_at);
        assert!(project.validate().is_ok());
        project.description = "é".repeat(MAX_PROJECT_DESCRIPTION_CHARS);
        assert!(project.validate().is_ok(), "the limit counts characters");
        project.description.push('x');
        assert!(project.validate().is_err());
    }

    #[test]
    fn project_serializes_rfc3339_and_snake_case() {
        let name = ProjectName::parse("payments").expect("name");
        let mut project = Project::new(ProjectId::generate(), name, at());
        project.visibility = ProjectVisibility::Public;
        let json = round_trip_json(&project);
        assert_eq!(json["visibility"], "public");
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["name"], "payments");
    }

    /// The matrix of the design document, written out independently of `template`.
    fn expected_content(role: ProjectRole, action: Action) -> bool {
        match action {
            Action::Browse | Action::Read => true,
            Action::Write => role >= ProjectRole::Developer,
            Action::Delete => role >= ProjectRole::Maintainer,
        }
    }

    fn expected_project(role: ProjectRole, permission: ProjectPermission) -> bool {
        use ProjectPermission::{
            ApplyLabels, ApprovePromotion, ManageLabels, ManageMembers, ManagePolicies,
            ManageRobots, ManageSettings, ManageWebhooks, RunScans, ViewAudit, ViewFindings,
            ViewProject, ViewQuota,
        };
        match permission {
            ViewProject | ViewQuota => true,
            ViewFindings | ViewAudit => role >= ProjectRole::Guest,
            RunScans | ApplyLabels => role >= ProjectRole::Developer,
            ManagePolicies | ManageWebhooks | ApprovePromotion => role >= ProjectRole::Maintainer,
            ManageLabels | ManageRobots | ManageMembers | ManageSettings => {
                role == ProjectRole::ProjectAdmin
            }
        }
    }

    #[test]
    fn project_role_matrix_is_pinned() {
        assert_eq!(
            ProjectPermission::ALL.len(),
            13,
            "a new permission needs a matrix row"
        );
        for role in ProjectRole::ALL {
            let grants = role.template();
            for action in [Action::Browse, Action::Read, Action::Write, Action::Delete] {
                assert_eq!(
                    grants.allows_content(action),
                    expected_content(role, action),
                    "{role:?} content {action:?}"
                );
            }
            for permission in ProjectPermission::ALL {
                assert_eq!(
                    grants.allows_project(*permission),
                    expected_project(role, *permission),
                    "{role:?} {permission:?}"
                );
            }
        }
    }

    /// No wildcard arm: a new permission has to be added here and to `ALL`.
    fn permission_position(permission: ProjectPermission) -> usize {
        match permission {
            ProjectPermission::ViewProject => 0,
            ProjectPermission::ViewQuota => 1,
            ProjectPermission::ViewFindings => 2,
            ProjectPermission::ViewAudit => 3,
            ProjectPermission::RunScans => 4,
            ProjectPermission::ApplyLabels => 5,
            ProjectPermission::ManageLabels => 6,
            ProjectPermission::ManagePolicies => 7,
            ProjectPermission::ManageWebhooks => 8,
            ProjectPermission::ManageRobots => 9,
            ProjectPermission::ManageMembers => 10,
            ProjectPermission::ManageSettings => 11,
            ProjectPermission::ApprovePromotion => 12,
        }
    }

    #[test]
    fn all_lists_every_permission_exactly_once_in_order() {
        for (index, permission) in ProjectPermission::ALL.iter().enumerate() {
            assert_eq!(permission_position(*permission), index, "{permission:?}");
        }
        assert_eq!(ProjectPermission::ALL.len(), 13);
    }

    #[test]
    fn roles_are_strictly_nested() {
        for pair in ProjectRole::ALL.windows(2) {
            let (lower, higher) = (pair[0].template(), pair[1].template());
            assert!(lower.content.iter().all(|a| higher.allows_content(*a)));
            assert!(lower.project.iter().all(|p| higher.allows_project(*p)));
            assert!(
                higher.content.len() + higher.project.len()
                    > lower.content.len() + lower.project.len(),
                "{pair:?} must grant strictly more"
            );
        }
    }

    #[test]
    fn role_labels_round_trip_and_match_the_database_check() {
        for role in ProjectRole::ALL {
            assert_eq!(ProjectRole::parse(role.as_str()).expect("parse"), role);
            assert_wire(&role, &format!("\"{}\"", role.as_str()));
        }
        assert!(ProjectRole::parse("owner").is_err());
        assert!(ProjectRole::parse("Guest").is_err());
        let sql_values = [
            "limited_guest",
            "guest",
            "developer",
            "maintainer",
            "project_admin",
        ];
        let labels: Vec<_> = ProjectRole::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(labels, sql_values);
    }

    #[test]
    fn permission_wire_names_are_snake_case() {
        assert_wire(
            &ProjectPermission::ApprovePromotion,
            "\"approve_promotion\"",
        );
        assert_wire(&ProjectPermission::ViewAudit, "\"view_audit\"");
    }

    #[test]
    fn subjects_are_internally_tagged() {
        let user = UserId::generate();
        assert_wire(
            &ProjectSubject::User { user_id: user },
            &format!(r#"{{"kind":"user","user_id":"{user}"}}"#),
        );
        assert_wire(
            &ProjectSubject::Group {
                provider: IdpProvider::Oidc,
                external_group: "/teams/Payments".into(),
            },
            r#"{"kind":"group","provider":"oidc","external_group":"/teams/Payments"}"#,
        );
    }

    #[test]
    fn group_strings_are_never_normalized() {
        let padded = " /Teams/X ";
        let member = ProjectMember::new(
            ProjectMemberId::generate(),
            ProjectId::generate(),
            ProjectSubject::Group {
                provider: IdpProvider::Ldap,
                external_group: padded.into(),
            },
            ProjectRole::Guest,
            at(),
        );
        assert!(member.validate().is_ok());
        let json = round_trip_json(&member);
        assert_eq!(json["subject"]["external_group"], padded);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    }

    #[test]
    fn group_subject_limits_are_enforced() {
        let make = |group: String| {
            ProjectMember::new(
                ProjectMemberId::generate(),
                ProjectId::generate(),
                ProjectSubject::Group {
                    provider: IdpProvider::Oidc,
                    external_group: group,
                },
                ProjectRole::Developer,
                at(),
            )
        };
        assert!(make(String::new()).validate().is_err());
        assert!(make("g".repeat(1024)).validate().is_ok());
        assert!(make("g".repeat(1025)).validate().is_err());
    }
}
