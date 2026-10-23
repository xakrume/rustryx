//! Robot tokens: automation access without a person.
//!
//! A robot has no roles, never `nx-admin`, no project permission except `scan`, and cannot log
//! in to the UI. Its rights are exactly its scopes (union of the grants), never the creator's
//! rights at use time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::DomainError;
use crate::ids::{ProjectId, RobotTokenId, UserId};
use crate::names::RepoName;
use crate::project::ProjectName;

/// Most scopes one robot may hold.
pub const MAX_ROBOT_SCOPES: usize = 64;
/// Longest full robot name (`<prefix>+<project>+<short name>`).
pub const MAX_ROBOT_NAME_LEN: usize = 128;
/// Longest short robot name.
pub const MAX_ROBOT_SHORT_NAME_LEN: usize = 64;

/// A robot token record. The secret is shown once and only its SHA-256 is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RobotToken {
    /// Identifier.
    pub id: RobotTokenId,
    /// Full display name, `<prefix>+<project>+<short name>` for a project robot and
    /// `<prefix>+<short name>` for a system robot. Informational: the Basic user name is
    /// ignored, exactly as for personal tokens.
    pub name: String,
    /// The part the creator chose.
    pub short_name: String,
    /// `None` for a system robot.
    pub project_id: Option<ProjectId>,
    /// Same grammar family as personal token scopes, see [`RobotScopeSet`]. At least one, at
    /// most 64.
    pub scopes: Vec<String>,
    /// Lowercase hex SHA-256 of the whole `ryx_` token. The secret is shown once.
    pub token_hash: String,
    /// First characters of the token, safe to display.
    pub display_prefix: String,
    /// `None` only for a never-expire robot, which needs `nx-admin` and
    /// `robots.allow_never_expire`.
    #[serde(default, with = "crate::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Set while the robot is disabled (rejected, but kept).
    #[serde(default, with = "crate::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
    /// Revocation time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// Last successful use (coalesced to once a minute per node).
    #[serde(default, with = "crate::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    /// Last secret rotation.
    #[serde(default, with = "crate::rfc3339::option")]
    pub rotated_at: Option<OffsetDateTime>,
    /// Creator, `None` once that account is deleted (the robot survives it).
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl RobotToken {
    /// A robot with no expiry, no flags set and version 1. Assign `expires_at`,
    /// `project_id` and `created_by` afterwards.
    #[must_use]
    pub fn new(
        id: RobotTokenId,
        name: String,
        short_name: String,
        scopes: Vec<String>,
        token_hash: String,
        display_prefix: String,
        created_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name,
            short_name,
            project_id: None,
            scopes,
            token_hash,
            display_prefix,
            expires_at: None,
            disabled_at: None,
            revoked_at: None,
            last_used_at: None,
            rotated_at: None,
            created_by: None,
            created_at,
            version: 1,
        }
    }

    /// True while the token may authenticate: not revoked, not disabled and not past its
    /// expiry (an expiry equal to `now` already counts as expired). A robot without an expiry
    /// never expires.
    #[must_use]
    pub fn is_usable_at(&self, now: OffsetDateTime) -> bool {
        self.revoked_at.is_none()
            && self.disabled_at.is_none()
            && self.expires_at.is_none_or(|expiry| expiry > now)
    }

    /// Builds the full display name: `<prefix>+<project>+<short name>` for a project robot,
    /// `<prefix>+<short name>` for a system robot.
    #[must_use]
    pub fn full_name(prefix: &str, project: Option<&ProjectName>, short_name: &str) -> String {
        match project {
            Some(project) => format!("{prefix}+{project}+{short_name}"),
            None => format!("{prefix}+{short_name}"),
        }
    }

    /// Checks the short name: 1 to 64 characters of ASCII letters, digits, `.`, `_` and `-`,
    /// the first one a letter or digit (the `robot_tokens.short_name` constraint).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the rule is violated.
    pub fn validate_short_name(short_name: &str) -> Result<(), DomainError> {
        let ok = !short_name.is_empty()
            && short_name.len() <= MAX_ROBOT_SHORT_NAME_LEN
            && short_name.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphanumeric() || (i > 0 && matches!(b, b'.' | b'_' | b'-'))
            });
        if ok {
            Ok(())
        } else {
            Err(DomainError::invalid(
                "robot name must be 1 to 64 letters, digits, '.', '_' or '-', starting with a letter or digit",
            ))
        }
    }

    /// Checks the full name: 1 to 128 characters of ASCII letters, digits, `+`, `.`, `_` and
    /// `-`, the first one a letter or digit (the `robot_tokens.name` constraint).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the rule is violated.
    pub fn validate_name(name: &str) -> Result<(), DomainError> {
        let ok = !name.is_empty()
            && name.len() <= MAX_ROBOT_NAME_LEN
            && name.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphanumeric() || (i > 0 && matches!(b, b'+' | b'.' | b'_' | b'-'))
            });
        if ok {
            Ok(())
        } else {
            Err(DomainError::invalid(
                "robot full name must be 1 to 128 letters, digits, '+', '.', '_' or '-', starting with a letter or digit",
            ))
        }
    }
}

/// Input for creating a robot. It carries no secret: the service generates the token, stores
/// its hash and shows the token once.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewRobotToken {
    /// The part of the name the creator chose.
    pub short_name: String,
    /// `None` for a system robot.
    pub project_id: Option<ProjectId>,
    /// The rights, already shape checked.
    pub scopes: RobotScopeSet,
    /// `None` asks for a never-expire robot (needs `nx-admin` and the setting).
    pub expires_at: Option<OffsetDateTime>,
    /// The creating user.
    pub created_by: Option<UserId>,
}

impl NewRobotToken {
    /// A request without project, expiry or creator (assign them as needed).
    #[must_use]
    pub fn new(short_name: impl Into<String>, scopes: RobotScopeSet) -> Self {
        Self {
            short_name: short_name.into(),
            project_id: None,
            scopes,
            expires_at: None,
            created_by: None,
        }
    }

    /// Turns the request into the stored record once the service has generated the secret.
    #[must_use]
    pub fn into_token(
        self,
        id: RobotTokenId,
        name: String,
        token_hash: String,
        display_prefix: String,
        now: OffsetDateTime,
    ) -> RobotToken {
        let mut token = RobotToken::new(
            id,
            name,
            self.short_name,
            self.scopes.into_vec(),
            token_hash,
            display_prefix,
            now,
        );
        token.project_id = self.project_id;
        token.expires_at = self.expires_at;
        token.created_by = self.created_by;
        token
    }
}

/// A validated scope list for a robot.
///
/// Parsing enforces: no `*`, at least one scope (at most 64), only the actions
/// `browse|read|write|delete` on `repo:` scopes and those plus `scan` on `project:` scopes, no
/// `*` action, and "push implies pull": `read` requires `browse` and `write` or `delete`
/// require `read` on the SAME target (`repo:*:read` does not satisfy `repo:app:write`). A
/// project id in a `project:` scope is the canonical lowercase hyphenated UUID, because ids,
/// not names, are what a token names (renaming a project does not change what a token can
/// reach). A project robot may carry only `project:<its project id>:...` scopes and `repo:`
/// scopes of repositories in that project: the service checks that, the type only checks the
/// shape.
///
/// ```text
/// scope   = "*" | "repo:" target ":" action | "project:" ptarget ":" action
/// target  = repository name | "*"
/// ptarget = project id (UUID) | "*"
/// action  = "browse" | "read" | "write" | "delete" | "scan" | "*"      "scan" only on project scopes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotScopeSet(Vec<String>);

const BROWSE: u8 = 1;
const READ: u8 = 2;
const WRITE: u8 = 4;
const DELETE: u8 = 8;

impl RobotScopeSet {
    /// Validates and normalizes (drops exact duplicates, keeps the order) a scope list.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the index of the first offending scope, or the
    /// rule that the set as a whole breaks.
    pub fn parse(scopes: &[String]) -> Result<Self, DomainError> {
        if scopes.is_empty() {
            return Err(DomainError::invalid("a robot needs at least one scope"));
        }
        if scopes.len() > MAX_ROBOT_SCOPES {
            return Err(DomainError::invalid("a robot may hold at most 64 scopes"));
        }
        let mut kept: Vec<String> = Vec::with_capacity(scopes.len());
        let mut actions: BTreeMap<(bool, &str), u8> = BTreeMap::new();
        for (index, raw) in scopes.iter().enumerate() {
            let parsed = parse_scope(raw).map_err(|why| {
                DomainError::invalid(format!("scope {} is invalid: {why}", index + 1))
            })?;
            *actions
                .entry((parsed.is_project, parsed.target))
                .or_insert(0) |= parsed.bit;
            if !kept.contains(raw) {
                kept.push(raw.clone());
            }
        }
        for bits in actions.values() {
            if bits & READ != 0 && bits & BROWSE == 0 {
                return Err(DomainError::invalid(
                    "a robot scope with read needs browse on the same target",
                ));
            }
            if bits & (WRITE | DELETE) != 0 && bits & READ == 0 {
                return Err(DomainError::invalid(
                    "a robot scope with write or delete needs read on the same target (push implies pull)",
                ));
            }
        }
        Ok(Self(kept))
    }

    /// Borrows the validated scopes.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Consumes the set.
    #[must_use]
    pub fn into_vec(self) -> Vec<String> {
        self.0
    }
}

struct ParsedScope<'a> {
    is_project: bool,
    target: &'a str,
    /// The action bit; `scan` has no bit because it has no prerequisite and implies nothing.
    bit: u8,
}

fn parse_scope(raw: &str) -> Result<ParsedScope<'_>, &'static str> {
    if raw == "*" {
        return Err("a robot cannot hold '*'");
    }
    let mut parts = raw.splitn(3, ':');
    let (Some(kind), Some(target), Some(action), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("expected kind:target:action");
    };
    let is_project = match kind {
        "repo" => false,
        "project" => true,
        _ => return Err("kind must be 'repo' or 'project'"),
    };
    if is_project {
        validate_project_target(target)?;
    } else if target != "*" && RepoName::parse(target).is_err() {
        return Err("repository target must be a repository name or '*'");
    }
    let bit = match action {
        "browse" => BROWSE,
        "read" => READ,
        "write" => WRITE,
        "delete" => DELETE,
        "scan" if is_project => 0,
        "scan" => return Err("'scan' is only valid on a project scope"),
        "*" => return Err("a robot cannot hold the '*' action"),
        _ => return Err("unknown action"),
    };
    Ok(ParsedScope {
        is_project,
        target,
        bit,
    })
}

fn validate_project_target(target: &str) -> Result<(), &'static str> {
    if target == "*" {
        return Ok(());
    }
    match Uuid::parse_str(target) {
        Ok(id) if id.hyphenated().to_string() == target => Ok(()),
        _ => Err("project target must be a lowercase hyphenated project id or '*'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{at, round_trip_json};

    fn scopes(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn pid() -> String {
        ProjectId::generate().to_string()
    }

    #[test]
    fn accepts_push_and_pull_scopes() {
        let project = pid();
        let set = RobotScopeSet::parse(&scopes(&[
            "repo:app:browse",
            "repo:app:read",
            "repo:app:write",
            "repo:*:browse",
            "repo:*:read",
            &format!("project:{project}:browse"),
            &format!("project:{project}:read"),
            &format!("project:{project}:scan"),
            "project:*:browse",
            "project:*:read",
            "project:*:write",
            "project:*:delete",
        ]))
        .expect("valid set");
        assert_eq!(set.as_slice().len(), 12);
    }

    #[test]
    fn must_not_accept_write_without_read_in_a_robot_scope() {
        for broken in [
            scopes(&["repo:app:write"]),
            scopes(&["repo:app:browse", "repo:app:write"]),
            scopes(&["repo:app:delete"]),
            scopes(&[
                "repo:app:browse",
                "repo:app:read",
                "repo:app:write",
                "repo:other:delete",
            ]),
            scopes(&["project:*:write"]),
        ] {
            let error = RobotScopeSet::parse(&broken).expect_err("push implies pull");
            assert!(error.to_string().contains("read"), "{error}");
        }
    }

    #[test]
    fn must_not_accept_read_without_browse_in_a_robot_scope() {
        assert!(RobotScopeSet::parse(&scopes(&["repo:app:read"])).is_err());
        assert!(RobotScopeSet::parse(&scopes(&["project:*:read"])).is_err());
    }

    #[test]
    fn prerequisites_are_per_target_not_across_targets() {
        // `repo:*` read access does not cover the explicit target of the write scope.
        let broken = scopes(&["repo:*:browse", "repo:*:read", "repo:app:write"]);
        assert!(RobotScopeSet::parse(&broken).is_err());
        // The same actions on a project scope do not satisfy a repository scope either.
        let project = pid();
        let mixed = scopes(&[
            &format!("project:{project}:browse"),
            &format!("project:{project}:read"),
            "repo:app:write",
        ]);
        assert!(RobotScopeSet::parse(&mixed).is_err());
    }

    #[test]
    fn must_not_accept_a_wildcard_scope_or_action() {
        assert!(RobotScopeSet::parse(&scopes(&["*"])).is_err());
        assert!(RobotScopeSet::parse(&scopes(&["repo:app:*"])).is_err());
        assert!(RobotScopeSet::parse(&scopes(&["project:*:*"])).is_err());
    }

    #[test]
    fn scan_is_valid_only_on_a_project_scope() {
        assert!(RobotScopeSet::parse(&scopes(&["project:*:scan"])).is_ok());
        assert!(RobotScopeSet::parse(&scopes(&["repo:app:scan"])).is_err());
    }

    #[test]
    fn rejects_malformed_scopes() {
        let project = pid();
        let upper = project.to_uppercase();
        let braced = format!("{{{project}}}");
        for bad in [
            "",
            "repo",
            "repo:app",
            "repo:app:browse:extra",
            "repo::browse",
            "repo:../etc:browse",
            "repo:a b:browse",
            "repo:app:Browse",
            "repo:app:admin",
            "image:app:read",
            "project:payments:read",
            "project::browse",
            &format!("project:{upper}:browse"),
            &format!("project:{braced}:browse"),
            "project:12345:browse",
        ] {
            assert!(
                RobotScopeSet::parse(&scopes(&[bad])).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn at_least_one_and_at_most_64_scopes() {
        assert!(RobotScopeSet::parse(&[]).is_err());
        let many: Vec<String> = (0..64).map(|i| format!("repo:r{i}:browse")).collect();
        assert!(RobotScopeSet::parse(&many).is_ok());
        let too_many: Vec<String> = (0..65).map(|i| format!("repo:r{i}:browse")).collect();
        assert!(RobotScopeSet::parse(&too_many).is_err());
    }

    #[test]
    fn duplicates_collapse_and_order_is_kept() {
        let set = RobotScopeSet::parse(&scopes(&[
            "repo:b:browse",
            "repo:a:browse",
            "repo:b:browse",
        ]))
        .expect("valid");
        assert_eq!(set.as_slice(), scopes(&["repo:b:browse", "repo:a:browse"]));
        assert_eq!(set.clone().into_vec().len(), 2);
    }

    #[test]
    fn error_messages_name_the_index_not_the_scope_text() {
        let error =
            RobotScopeSet::parse(&scopes(&["repo:a:browse", "token=abc123"])).expect_err("invalid");
        let text = error.to_string();
        assert!(text.contains("scope 2"), "{text}");
        assert!(!text.contains("abc123"), "{text}");
    }

    #[test]
    fn names_follow_the_database_constraints() {
        for ok in ["ci", "a", "ci-deploy_1.x", &"a".repeat(64)] {
            assert!(RobotToken::validate_short_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-ci", ".ci", "ci+x", "ci x", &"a".repeat(65)] {
            assert!(RobotToken::validate_short_name(bad).is_err(), "{bad}");
        }
        for ok in ["robot+ci", "robot+payments+ci", &"a".repeat(128)] {
            assert!(RobotToken::validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "+robot", "robot ci", "robot/ci", &"a".repeat(129)] {
            assert!(RobotToken::validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn full_name_has_the_documented_shape() {
        let project = ProjectName::parse("payments").expect("name");
        assert_eq!(
            RobotToken::full_name("robot", Some(&project), "ci"),
            "robot+payments+ci"
        );
        assert_eq!(RobotToken::full_name("robot", None, "ci"), "robot+ci");
        assert!(RobotToken::validate_name("robot+payments+ci").is_ok());
    }

    fn sample(now: OffsetDateTime) -> RobotToken {
        RobotToken::new(
            RobotTokenId::generate(),
            "robot+ci".into(),
            "ci".into(),
            scopes(&["repo:app:browse"]),
            "0".repeat(64),
            "ryx_ab".into(),
            now,
        )
    }

    #[test]
    fn constructor_defaults_and_usability() {
        let token = sample(at());
        assert_eq!(token.version, 1);
        assert!(token.project_id.is_none() && token.expires_at.is_none());
        assert!(token.is_usable_at(at()), "no expiry never expires");
    }

    #[test]
    fn must_not_use_a_disabled_revoked_or_expired_robot() {
        let now = at();
        let hour = time::Duration::hours(1);

        let mut disabled = sample(now);
        disabled.disabled_at = Some(now);
        assert!(!disabled.is_usable_at(now));

        let mut revoked = sample(now);
        revoked.revoked_at = Some(now);
        assert!(!revoked.is_usable_at(now));

        let mut expiring = sample(now);
        expiring.expires_at = Some(now + hour);
        assert!(expiring.is_usable_at(now));
        assert!(
            !expiring.is_usable_at(now + hour),
            "expiry at `now` is expired"
        );
        assert!(!expiring.is_usable_at(now + hour + hour));
    }

    #[test]
    fn new_robot_request_builds_the_record() {
        let set = RobotScopeSet::parse(&scopes(&["repo:app:browse"])).expect("scopes");
        let mut request = NewRobotToken::new("ci", set);
        request.project_id = Some(ProjectId::generate());
        request.expires_at = Some(at() + time::Duration::days(30));
        request.created_by = Some(UserId::generate());
        let token = request.clone().into_token(
            RobotTokenId::generate(),
            "robot+p+ci".into(),
            "a".repeat(64),
            "ryx_".into(),
            at(),
        );
        assert_eq!(token.short_name, "ci");
        assert_eq!(token.scopes, scopes(&["repo:app:browse"]));
        assert_eq!(token.project_id, request.project_id);
        assert_eq!(token.expires_at, request.expires_at);
        assert_eq!(token.created_by, request.created_by);
    }

    #[test]
    fn robot_token_serializes_rfc3339_and_keeps_only_the_hash() {
        let mut token = sample(at());
        token.expires_at = Some(at() + time::Duration::days(30));
        let json = round_trip_json(&token);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["expires_at"], "2026-11-23T10:00:00Z");
        assert!(json["revoked_at"].is_null());
        assert_eq!(json["token_hash"], "0".repeat(64));
        assert!(json.get("token").is_none() && json.get("secret").is_none());
    }
}
