//! Dependency-confusion protection for groups.
//!
//! The attack: an internal package `acme-billing` lives in a hosted member. An
//! attacker publishes `acme-billing 99.0.0` on the public registry that a proxy
//! member mirrors. If the group merged both, every client would prefer the higher
//! version and install the attacker's code.
//!
//! The defence is a *claim*: when a hosted member has any component with a package
//! name, that name belongs to the hosted members of the group. Proxy members that
//! come later in the group order are not asked about it at all, for index requests
//! and for artifact requests alike, so neither the version list nor a direct
//! download URL can reach the public copy.
//!
//! Two properties are deliberate:
//! * The claim is evaluated against *all* hosted members, whatever the caller may
//!   read. A caller without rights on the hosted member must not be exposed to the
//!   public look-alike either; for them the package simply does not exist.
//! * A failing claim lookup counts as "claimed" (fail closed): an unreachable
//!   database must not open the door to the public copy.
//!
//! Order matters: only hosted members *before* a proxy member claim against it.
//! Put hosted members first.

use std::sync::Arc;

use rustryx_core::model::{RepoType, Repository};
use rustryx_core::ports::{ComponentFilter, PageRequest};

use crate::context::FormatContext;
use crate::request::ArtifactRequest;

/// What a request claims: components whose namespace and/or name match.
///
/// A `None` field matches anything, so a claim on `group` alone protects a whole
/// namespace (Maven groupId). At least one field should be set; a claim with both
/// `None` claims everything and is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameClaim {
    /// Namespace (maven groupId, npm scope).
    pub group: Option<String>,
    /// Package name, in the same canonical form components are stored in.
    pub name: Option<String>,
}

impl NameClaim {
    /// Claim on a package name without namespace.
    #[must_use]
    pub fn package(name: impl Into<String>) -> Self {
        Self {
            group: None,
            name: Some(name.into()),
        }
    }

    /// Claim on a package inside a namespace (npm scope).
    #[must_use]
    pub fn scoped(group: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            group: Some(group.into()),
            name: Some(name.into()),
        }
    }

    /// Claim on a whole namespace (maven groupId).
    #[must_use]
    pub fn namespace(group: impl Into<String>) -> Self {
        Self {
            group: Some(group.into()),
            name: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.group.is_none() && self.name.is_none()
    }
}

/// Extracts the package a request addresses, so the group can apply claims.
///
/// Implemented by formats that have package names. The returned values must be in
/// the canonical form the format stores components in (`PyPI`: PEP 503 normalized
/// name; npm: lowercase scope and name), otherwise a look-alike spelling escapes the claim.
pub trait GroupNameClaimer: Send + Sync {
    /// The claim for this request, or `None` when it does not address one package
    /// (a root listing, a search).
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim>;
}

/// For each member (same order), whether it is a hosted member that holds `claim`.
pub(crate) async fn holders(
    cx: &FormatContext,
    members: &[Arc<Repository>],
    claim: &NameClaim,
) -> Vec<bool> {
    if claim.is_empty() {
        return vec![false; members.len()];
    }
    let lookups = members.iter().map(|member| async move {
        if member.repo_type != RepoType::Hosted {
            return false;
        }
        let filter = ComponentFilter {
            repository_id: Some(member.id),
            group_id: claim.group.clone(),
            name: claim.name.clone(),
            ..ComponentFilter::default()
        };
        let page = PageRequest {
            limit: 1,
            after: None,
        };
        match cx.ports().components.list(&filter, &page).await {
            Ok(found) => !found.items.is_empty(),
            Err(error) => {
                tracing::warn!(%error, member = %member.name, "name claim lookup failed; treating the name as claimed");
                true
            }
        }
    });
    futures::future::join_all(lookups).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_fill_the_right_fields() {
        assert_eq!(NameClaim::package("a").name.as_deref(), Some("a"));
        assert!(NameClaim::package("a").group.is_none());
        let scoped = NameClaim::scoped("s", "n");
        assert_eq!(
            (scoped.group.as_deref(), scoped.name.as_deref()),
            (Some("s"), Some("n"))
        );
        let ns = NameClaim::namespace("g");
        assert!(ns.name.is_none());
        assert!(!ns.is_empty());
        assert!(
            NameClaim {
                group: None,
                name: None
            }
            .is_empty()
        );
    }
}
