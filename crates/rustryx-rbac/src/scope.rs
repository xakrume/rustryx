//! API token scopes.
//!
//! A token carries a list of scope strings. Scopes can only narrow what the
//! owner may do, never widen it: the effective right of a token request is the
//! intersection of the owner's privileges and the token's scopes.
//!
//! Syntax of one scope:
//!
//! ```text
//! scope  = "*" | "repo:" target ":" action
//! target = repository name | "*"
//! action = "browse" | "read" | "write" | "delete" | "*"
//! ```
//!
//! `*` lifts the restriction (the token has the owner's rights). A list of
//! scopes is a union: the request must be covered by at least one of them.
//! Matching is exact and case sensitive; there is no prefix or glob matching
//! other than the lone `*`. Repository names cannot contain `:` (see
//! `RepoName`), so splitting on it is unambiguous.
//!
//! A principal whose `token_scopes` is `None` or empty is not restricted (the
//! `ApiToken` documentation: "empty means owner's rights"). A non-empty list
//! whose entries are all malformed restricts everything: a malformed scope is
//! ignored, never guessed, so a typo narrows the token instead of widening it.

use rustryx_core::identity::{Action, Principal};

/// One parsed scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `*`: no restriction.
    Unrestricted,
    /// `repo:<target>:<action>`.
    Repository {
        /// Repository name, or `None` for `*`.
        repository: Option<String>,
        /// Action, or `None` for `*`.
        action: Option<Action>,
    },
}

/// A scope string that does not follow the syntax.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid token scope '{scope}': {reason}")]
pub struct ScopeError {
    scope: String,
    reason: &'static str,
}

fn action_from_label(label: &str) -> Option<Action> {
    match label {
        "browse" => Some(Action::Browse),
        "read" => Some(Action::Read),
        "write" => Some(Action::Write),
        "delete" => Some(Action::Delete),
        _ => None,
    }
}

/// Parses one scope string. Token creation uses this to reject bad scopes up
/// front; authorization uses it and ignores what fails.
pub fn parse_scope(scope: &str) -> Result<Scope, ScopeError> {
    let fail = |reason| ScopeError {
        scope: scope.to_owned(),
        reason,
    };
    if scope == "*" {
        return Ok(Scope::Unrestricted);
    }
    let mut parts = scope.split(':');
    let (Some("repo"), Some(target), Some(action), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(fail("expected '*' or 'repo:<repository>:<action>'"));
    };
    if target.is_empty() {
        return Err(fail(
            "the repository is empty, use '*' for all repositories",
        ));
    }
    let action = if action == "*" {
        None
    } else {
        Some(
            action_from_label(action)
                .ok_or_else(|| fail("the action must be browse, read, write, delete or '*'"))?,
        )
    };
    Ok(Scope::Repository {
        repository: (target != "*").then(|| target.to_owned()),
        action,
    })
}

/// The restriction a request's token imposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenScopes {
    unrestricted: bool,
    rules: Vec<(Option<String>, Option<Action>)>,
    ignored: usize,
}

impl TokenScopes {
    /// No restriction (a session, or a token without scopes).
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            unrestricted: true,
            rules: Vec::new(),
            ignored: 0,
        }
    }

    /// Reads the scopes of the principal's API token.
    #[must_use]
    pub fn from_principal(principal: &Principal) -> Self {
        Self::parse(principal.token_scopes.as_deref())
    }

    /// Parses a scope list. `None` and an empty list mean no restriction.
    #[must_use]
    pub fn parse(scopes: Option<&[String]>) -> Self {
        let Some(scopes) = scopes.filter(|s| !s.is_empty()) else {
            return Self::unrestricted();
        };
        let mut parsed = Self {
            unrestricted: false,
            rules: Vec::new(),
            ignored: 0,
        };
        for scope in scopes {
            match parse_scope(scope) {
                Ok(Scope::Unrestricted) => parsed.unrestricted = true,
                Ok(Scope::Repository { repository, action }) => {
                    parsed.rules.push((repository, action));
                }
                Err(_) => parsed.ignored += 1,
            }
        }
        parsed
    }

    /// True when the token does not restrict anything.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted
    }

    /// Number of malformed scopes that were ignored.
    #[must_use]
    pub fn ignored(&self) -> usize {
        self.ignored
    }

    /// True when the scopes cover `action` on `repository`.
    #[must_use]
    pub fn allows(&self, action: Action, repository: &str) -> bool {
        self.unrestricted
            || self.rules.iter().any(|(repo, act)| {
                repo.as_deref().is_none_or(|r| r == repository) && act.is_none_or(|a| a == action)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(list: &[&str]) -> TokenScopes {
        let owned: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
        TokenScopes::parse(Some(&owned))
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse_scope("*").unwrap(), Scope::Unrestricted);
        assert_eq!(
            parse_scope("repo:maven-releases:read").unwrap(),
            Scope::Repository {
                repository: Some("maven-releases".into()),
                action: Some(Action::Read)
            }
        );
        assert_eq!(
            parse_scope("repo:*:*").unwrap(),
            Scope::Repository {
                repository: None,
                action: None
            }
        );
        for (label, action) in [
            ("browse", Action::Browse),
            ("read", Action::Read),
            ("write", Action::Write),
            ("delete", Action::Delete),
        ] {
            assert_eq!(
                parse_scope(&format!("repo:r:{label}")).unwrap(),
                Scope::Repository {
                    repository: Some("r".into()),
                    action: Some(action)
                }
            );
        }
    }

    #[test]
    fn rejects_malformed_scopes() {
        for bad in [
            "",
            "repo",
            "repo:r",
            "repo::read",
            "repo:r:read:extra",
            "repo:r:admin",
            "repo:r:READ",
            "Repo:r:read",
            "read",
            "**",
            "repo:r:",
            " *",
        ] {
            assert!(parse_scope(bad).is_err(), "{bad:?}");
        }
        assert!(
            parse_scope("repo:r:admin")
                .unwrap_err()
                .to_string()
                .contains("repo:r:admin")
        );
    }

    #[test]
    fn none_and_empty_mean_unrestricted() {
        assert!(TokenScopes::parse(None).is_unrestricted());
        assert!(TokenScopes::parse(Some(&[])).is_unrestricted());
        assert!(TokenScopes::unrestricted().allows(Action::Delete, "any"));
    }

    #[test]
    fn scopes_are_a_union_with_exact_matching() {
        let s = scopes(&["repo:a:read", "repo:b:*", "repo:*:browse"]);
        assert!(s.allows(Action::Read, "a"));
        assert!(!s.allows(Action::Write, "a"));
        assert!(s.allows(Action::Delete, "b"));
        assert!(s.allows(Action::Browse, "anything"));
        assert!(!s.allows(Action::Read, "anything"));
        assert!(!s.allows(Action::Read, "a2"), "no prefix matching");
        assert!(!s.allows(Action::Read, "A"), "case sensitive");
        assert!(!s.is_unrestricted());
    }

    #[test]
    fn star_in_a_list_lifts_the_restriction() {
        assert!(scopes(&["repo:a:read", "*"]).allows(Action::Delete, "z"));
    }

    #[test]
    fn malformed_entries_are_ignored_and_never_widen() {
        let s = scopes(&["repo:a:reed", "garbage"]);
        assert_eq!(s.ignored(), 2);
        assert!(!s.allows(Action::Read, "a"));
        assert!(!s.allows(Action::Browse, "a"));
        let mixed = scopes(&["repo:a:read", "garbage"]);
        assert_eq!(mixed.ignored(), 1);
        assert!(mixed.allows(Action::Read, "a"));
    }

    #[test]
    fn reads_scopes_from_the_principal() {
        let mut p = Principal::anonymous();
        assert!(TokenScopes::from_principal(&p).is_unrestricted());
        p.token_scopes = Some(vec!["repo:a:read".into()]);
        let s = TokenScopes::from_principal(&p);
        assert!(s.allows(Action::Read, "a"));
        assert!(!s.allows(Action::Read, "b"));
    }
}
