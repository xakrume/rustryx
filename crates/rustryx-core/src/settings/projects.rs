//! The projects section: project creation, the group mapping policy and robot token rules.

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::identity::IdpProvider;

/// Who may create projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectCreation {
    /// Any authenticated user.
    Everyone,
    /// Only `nx-admin` (default): a tenant boundary is a conscious act.
    #[default]
    AdminsOnly,
}

/// Which group strings a project admin may map to a project role. A group row gives access to
/// people the project admin does not manage, so the default is that only `nx-admin` adds one.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct GroupMappingPolicy {
    /// Default false: only `nx-admin` adds group rows to a project.
    pub project_admins_may_map: bool,
    /// Keycloak group paths a project admin may map must start with one of these; each starts
    /// with `/`.
    pub oidc_path_prefixes: Vec<String>,
    /// LDAP group DNs a project admin may map must end with one of these.
    pub ldap_dn_suffixes: Vec<String>,
}

impl GroupMappingPolicy {
    /// True when a project admin (not `nx-admin`) may add a group row for `group`. Matching is
    /// exact and case sensitive on the whole string: no trimming, no case folding, no pattern.
    /// With `project_admins_may_map` false the answer is always no, and an empty list for the
    /// provider allows nothing.
    #[must_use]
    pub fn project_admin_may_map(&self, provider: IdpProvider, group: &str) -> bool {
        if !self.project_admins_may_map {
            return false;
        }
        match provider {
            IdpProvider::Oidc => self
                .oidc_path_prefixes
                .iter()
                .any(|p| group.starts_with(p.as_str())),
            IdpProvider::Ldap => self
                .ldap_dn_suffixes
                .iter()
                .any(|s| group.ends_with(s.as_str())),
        }
    }

    /// Checks that every OIDC prefix starts with `/` and that no entry is empty (an empty
    /// prefix or suffix would match every group).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.oidc_path_prefixes.iter().any(|p| !p.starts_with('/')) {
            return Err(DomainError::invalid(
                "an OIDC path prefix must start with '/'",
            ));
        }
        if self.ldap_dn_suffixes.iter().any(String::is_empty) {
            return Err(DomainError::invalid("an LDAP DN suffix must not be empty"));
        }
        Ok(())
    }
}

/// Rules for robot tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RobotSettings {
    /// First part of a robot name (`<prefix>+<project>+<name>`).
    pub name_prefix: String,
    /// Lifetime of a robot created without an explicit expiry, in days.
    pub default_ttl_days: u32,
    /// Longest lifetime a robot may be given, in days.
    pub max_ttl_days: u32,
    /// Allow a robot without expiry (needs `nx-admin`).
    pub allow_never_expire: bool,
    /// Days an expired or revoked robot is kept before the purge job removes it.
    pub expired_retention_days: u32,
}

impl Default for RobotSettings {
    /// Prefix `robot`, 30 days by default, 365 at most, never-expire off, 30 days of retention.
    fn default() -> Self {
        Self {
            name_prefix: "robot".to_owned(),
            default_ttl_days: 30,
            max_ttl_days: 365,
            allow_never_expire: false,
            expired_retention_days: 30,
        }
    }
}

impl RobotSettings {
    /// Checks the prefix (letters, digits, `.`, `_` and `-`, 1 to 32 characters, so the full
    /// name stays valid), `1 <= default_ttl_days <= max_ttl_days` and a retention of at least
    /// one day.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        let prefix_ok = !self.name_prefix.is_empty()
            && self.name_prefix.len() <= 32
            && self.name_prefix.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphanumeric() || (i > 0 && matches!(b, b'.' | b'_' | b'-'))
            });
        if !prefix_ok {
            return Err(DomainError::invalid(
                "robot prefix must be 1 to 32 letters, digits, '.', '_' or '-'",
            ));
        }
        if self.default_ttl_days == 0 || self.default_ttl_days > self.max_ttl_days {
            return Err(DomainError::invalid(
                "robot default lifetime must be between 1 day and the maximum",
            ));
        }
        if self.expired_retention_days == 0 {
            return Err(DomainError::invalid(
                "robot retention must be at least one day",
            ));
        }
        Ok(())
    }
}

/// The projects section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectsSettings {
    /// Who may create projects.
    pub creation: ProjectCreation,
    /// The group mapping policy for project admins.
    pub group_mapping: GroupMappingPolicy,
    /// Robot token rules.
    pub robots: RobotSettings,
}

impl ProjectsSettings {
    /// Validates the group mapping policy and the robot rules.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] from the first failing part.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.group_mapping.validate()?;
        self.robots.validate()
    }
}
