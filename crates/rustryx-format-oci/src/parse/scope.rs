//! Scope strings of the token endpoint.
//!
//! Grammar, from the Docker token authentication specification
//! (<https://distribution.github.io/distribution/spec/auth/token/>) and the `OAuth2` variant
//! (<https://distribution.github.io/distribution/spec/auth/oauth/>):
//!
//! ```text
//! scope  := "repository:" name ":" actions | "registry:catalog:*"
//! actions := action ("," action)*
//! action  := "pull" | "push" | "delete" | "*"
//! ```
//!
//! `name` is `<rustryx-repo>/<image path>`. A client may send several scopes, as repeated
//! `scope` parameters or separated by spaces inside one. An action this registry does not
//! know is dropped (it can never be granted); a scope that does not fit the grammar is an
//! error, so that nothing is ever silently widened or reinterpreted.

use std::fmt;

use rustryx_core::names::RepoName;

use super::name::{RepositoryName, split_full_name};

/// Longest scope string taken at all.
const MAX_SCOPE_LEN: usize = 512;
/// Most scopes in one token request.
pub const MAX_SCOPES: usize = 16;

/// Why a scope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// Not `type:name:actions`, or too long.
    #[error("malformed scope")]
    Malformed,
    /// A resource type other than `repository` and `registry`.
    #[error("unsupported scope type")]
    UnsupportedType,
    /// The repository name is invalid.
    #[error("invalid repository name in scope")]
    InvalidName,
    /// More than [`MAX_SCOPES`] scopes.
    #[error("too many scopes")]
    TooMany,
}

/// An action a token can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Read: manifests, blobs, tags, referrers.
    Pull,
    /// Write: blob uploads, manifest pushes.
    Push,
    /// Remove manifests, tags and blobs.
    Delete,
}

impl Action {
    /// The word used in scopes and in token claims.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
            Self::Delete => "delete",
        }
    }

    /// Parses one action word; `None` for an unknown one.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "pull" => Some(Self::Pull),
            "push" => Some(Self::Push),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// All actions, for `*`.
    pub const ALL: [Self; 3] = [Self::Pull, Self::Push, Self::Delete];
}

/// The resource a scope names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// An image: the Rustryx repository and the path inside it.
    Repository {
        /// The Rustryx repository.
        repository: RepoName,
        /// The image path.
        image: RepositoryName,
    },
    /// The registry catalog (`registry:catalog:*`).
    Catalog,
}

/// One parsed scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// What the scope names.
    pub resource: Resource,
    /// The requested actions, known ones only, deduplicated, in `Pull, Push, Delete` order.
    pub actions: Vec<Action>,
}

impl Scope {
    /// Parses one scope string.
    ///
    /// # Errors
    /// A [`ScopeError`].
    pub fn parse(text: &str) -> Result<Self, ScopeError> {
        if text.is_empty() || text.len() > MAX_SCOPE_LEN {
            return Err(ScopeError::Malformed);
        }
        let (kind, rest) = text.split_once(':').ok_or(ScopeError::Malformed)?;
        match kind {
            "repository" => {
                // The actions follow the LAST colon: a name has none, so this is unambiguous
                // and a stray colon in the name ends up in the name and fails validation.
                let (name, actions) = rest.rsplit_once(':').ok_or(ScopeError::Malformed)?;
                let (repository, image) =
                    split_full_name(name).map_err(|_| ScopeError::InvalidName)?;
                Ok(Self {
                    resource: Resource::Repository { repository, image },
                    actions: parse_actions(actions),
                })
            }
            "registry" if rest == "catalog:*" => Ok(Self {
                resource: Resource::Catalog,
                actions: vec![Action::Pull],
            }),
            "registry" => Err(ScopeError::Malformed),
            _ => Err(ScopeError::UnsupportedType),
        }
    }

    /// The docker name of a repository scope (`<repo>/<image>`).
    #[must_use]
    pub fn name(&self) -> Option<String> {
        match &self.resource {
            Resource::Repository { repository, image } => Some(format!("{repository}/{image}")),
            Resource::Catalog => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.resource {
            Resource::Catalog => f.write_str("registry:catalog:*"),
            Resource::Repository { repository, image } => {
                let actions: Vec<&str> = self.actions.iter().map(|a| a.as_str()).collect();
                write!(f, "repository:{repository}/{image}:{}", actions.join(","))
            }
        }
    }
}

/// Splits the values of the `scope` parameters (each may hold several scopes separated by
/// whitespace) and parses all of them.
///
/// # Errors
/// [`ScopeError::TooMany`] beyond [`MAX_SCOPES`], otherwise the error of the first bad scope.
pub fn parse_scopes<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<Scope>, ScopeError> {
    let mut scopes = Vec::new();
    for value in values {
        for part in value.split_ascii_whitespace() {
            if scopes.len() >= MAX_SCOPES {
                return Err(ScopeError::TooMany);
            }
            scopes.push(Scope::parse(part)?);
        }
    }
    Ok(scopes)
}

fn parse_actions(text: &str) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();
    for word in text.split(',') {
        let word = word.trim();
        if word == "*" {
            actions.extend(Action::ALL);
        } else if let Some(action) = Action::parse(word) {
            actions.push(action);
        }
        // An unknown word cannot be granted, so it is dropped rather than refused: a client
        // that asks for `pull,push,mount` still gets what it may have.
    }
    actions.sort();
    actions.dedup();
    actions
}

#[cfg(test)]
mod tests;
