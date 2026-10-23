//! Group discovery: direct membership plus a bounded walk through nested groups.
//!
//! The walk is a breadth first search with three independent brakes, so a malicious
//! or merely broken directory cannot make it run away:
//! - a depth limit (`max_depth`, 1 means direct groups only);
//! - a visited set keyed by a normalised DN, so a cycle (`A` in `B` in `A`) is cut the
//!   second time round even when the directory spells the DN differently;
//! - a cap on the total number of groups returned (`max_groups`).
//!
//! Hitting the depth or size limit is NOT an error: the groups found so far are
//! returned (a shorter list can only remove access) and the event is logged and
//! counted by the caller from [`WalkOutcome`]. A failed search, in contrast, aborts
//! the whole walk with [`WalkError`]: a half walked tree must never be passed on as
//! the user's groups.

use std::collections::HashSet;

use super::config::{GroupSettings, LdapGroupStrategy};
use super::dn::dn_key;
use super::error::LdapError;
use super::filter::Substitution;
use super::pool::PooledConnection;
use super::transport::{
    AttributeValues, LdapEntry, SearchRequest, SearchScope, TransportError, result_code,
};
use super::user::DirectoryUser;

/// Groups found and what, if anything, cut the walk short.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WalkOutcome {
    /// DNs as the directory returned them, first spelling kept, discovery order.
    pub(crate) groups: Vec<String>,
    /// Expansion stopped because `max_depth` was reached while groups were still open.
    pub(crate) depth_limit_hit: bool,
    /// Adding stopped because `max_groups` was reached.
    pub(crate) size_limit_hit: bool,
}

/// Why a walk was abandoned.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WalkError {
    #[error(transparent)]
    Ldap(#[from] LdapError),
    #[error("group attribute is returned in ranges, which is not supported")]
    RangedAttribute,
    #[error("the server cut the group search short at its own size limit")]
    ServerSizeLimit,
}

/// Attribute list meaning "no attributes, DNs only" (RFC 4511 section 4.5.1.8).
const NO_ATTRIBUTES: &str = "1.1";
/// Matches every entry; used to read one group by its DN.
const MATCH_ALL: &str = "(objectClass=*)";

/// Resolves the groups of `user`.
pub(crate) async fn walk(
    conn: &mut PooledConnection,
    settings: &GroupSettings,
    user: &DirectoryUser,
) -> Result<WalkOutcome, WalkError> {
    let mut visited = HashSet::new();
    let mut outcome = WalkOutcome::default();

    let direct = direct_groups(conn, settings, user).await?;
    let mut frontier = admit(direct, settings.max_groups, &mut visited, &mut outcome);
    let mut depth = 1u32;
    while settings.nested && !frontier.is_empty() && !outcome.size_limit_hit {
        if depth >= settings.max_depth {
            outcome.depth_limit_hit = true;
            break;
        }
        let mut candidates = Vec::new();
        for group in &frontier {
            candidates.extend(parent_groups(conn, settings, group).await?);
        }
        frontier = admit(candidates, settings.max_groups, &mut visited, &mut outcome);
        depth += 1;
    }
    Ok(outcome)
}

/// Adds the groups not seen before to the result, up to the size cap, and returns the
/// ones that were added (they are the next level to expand).
fn admit(
    candidates: Vec<String>,
    max_groups: usize,
    visited: &mut HashSet<String>,
    outcome: &mut WalkOutcome,
) -> Vec<String> {
    let mut added = Vec::new();
    for candidate in candidates {
        if !visited.insert(dn_key(&candidate)) {
            continue;
        }
        if outcome.groups.len() >= max_groups {
            outcome.size_limit_hit = true;
            break;
        }
        outcome.groups.push(candidate.clone());
        added.push(candidate);
    }
    added
}

async fn direct_groups(
    conn: &mut PooledConnection,
    settings: &GroupSettings,
    user: &DirectoryUser,
) -> Result<Vec<String>, WalkError> {
    match settings.strategy {
        LdapGroupStrategy::MemberOf => member_of_values(&user.entry, &settings.member_of_attribute),
        LdapGroupStrategy::ReverseMember => {
            let filter = settings.group_filter.render(Substitution {
                username: &user.username,
                dn: &user.dn,
            });
            search_group_dns(conn, settings, filter).await
        }
    }
}

async fn parent_groups(
    conn: &mut PooledConnection,
    settings: &GroupSettings,
    group_dn: &str,
) -> Result<Vec<String>, WalkError> {
    match settings.strategy {
        LdapGroupStrategy::MemberOf => read_group_member_of(conn, settings, group_dn).await,
        LdapGroupStrategy::ReverseMember => {
            let filter = settings.nested_group_filter.render(Substitution {
                username: "",
                dn: group_dn,
            });
            search_group_dns(conn, settings, filter).await
        }
    }
}

fn member_of_values(entry: &LdapEntry, attribute: &str) -> Result<Vec<String>, WalkError> {
    match entry.values(attribute) {
        AttributeValues::Absent => Ok(Vec::new()),
        AttributeValues::Complete(values) => Ok(values.into_iter().map(str::to_owned).collect()),
        AttributeValues::Ranged => Err(WalkError::RangedAttribute),
    }
}

/// Reads one group entry and returns the groups it is itself a member of.
///
/// A group that no longer exists (`noSuchObject`) has no parents, which is a fact and
/// not a failure: `memberOf` values can outlive the group they point to. Any other
/// failure aborts the walk.
async fn read_group_member_of(
    conn: &mut PooledConnection,
    settings: &GroupSettings,
    group_dn: &str,
) -> Result<Vec<String>, WalkError> {
    let request = SearchRequest {
        base: group_dn.to_owned(),
        scope: SearchScope::Base,
        filter: MATCH_ALL.to_owned(),
        attributes: vec![settings.member_of_attribute.clone()],
        size_limit: Some(1),
    };
    match conn.search(&request).await {
        Ok(response) => match response.entries.first() {
            Some(entry) => member_of_values(entry, &settings.member_of_attribute),
            None => Ok(Vec::new()),
        },
        Err(LdapError::Transport(TransportError::Server { code, .. }))
            if code == result_code::NO_SUCH_OBJECT =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

/// Searches the group tree and returns the DNs found.
///
/// The request asks for one entry more than `max_groups`. If the server stops at that
/// limit the surplus entry is what proves the cap was reached (the walk then reports
/// `size_limit_hit`); if the server stops EARLIER, it applied a limit of its own and
/// the answer is incomplete for a reason nobody chose, which fails the lookup.
async fn search_group_dns(
    conn: &mut PooledConnection,
    settings: &GroupSettings,
    filter: String,
) -> Result<Vec<String>, WalkError> {
    let limit = u32::try_from(settings.max_groups.saturating_add(1)).unwrap_or(u32::MAX);
    let request = SearchRequest {
        base: settings.group_base_dn.clone(),
        scope: SearchScope::Subtree,
        filter,
        attributes: vec![NO_ATTRIBUTES.to_owned()],
        size_limit: Some(limit),
    };
    let response = conn.search(&request).await?;
    if response.size_limit_exceeded && response.entries.len() <= settings.max_groups {
        return Err(WalkError::ServerSizeLimit);
    }
    Ok(response.entries.into_iter().map(|entry| entry.dn).collect())
}
