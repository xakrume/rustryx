//! Group resolution: the `GroupResolver` implementation and the five minute rule.
//!
//! Policy, in one place:
//! 1. A confirmed set younger than `groups_cache_ttl_secs` (at most 300) is served
//!    without asking the directory.
//! 2. Otherwise the directory is asked (user search, then the group walk, under the
//!    group deadline). Success replaces the confirmed set and its timestamp.
//! 3. If the directory cannot answer (error, timeout) the lookup is an
//!    `Err(GroupLookupFailed)`: the caller drops the roles of origin `ldap` and answers
//!    403. Never an empty list.
//! 4. Optional (`groups_stale_on_error`, default off): before failing, the previously
//!    confirmed set is served while it is at most five minutes old, measured from the
//!    last successful search. There is no 15 or 60 minute grace (README 8.5).
//! 5. If the directory answers that the user is gone (not found, or ambiguous), the
//!    confirmed set is deleted and the lookup fails at once, stale mode or not: "the
//!    user does not exist" is an answer, not an outage.
//!
//! Why (4) is off by default: the authentication chain keeps its own copy of the last
//! successful answer and reuses it for at most five minutes when a lookup fails. Both
//! timelines are measured from the directory's answer (`ExternalGroups::confirmed_at`), so
//! the two graces no longer add up whichever one serves the stale set, and the chain's copy
//! alone already bounds the total at five minutes. The resolver stays strict anyway: a
//! stale `Ok` from here would hide the outage from the chain's failure handling and log.
//! The only state used to decide (4) is this resolver's own cache; the principal's
//! `groups_confirmed_at` is never trusted for it. Callers that want the freshness class as
//! well use [`LdapGroupResolver::resolve_detailed`].
//!
//! The user search of a refresh is skipped when the user's DN is known (`Principal::external_id`,
//! or the DN this cache remembers from the last lookup): the entry is read directly, through
//! the same user filter, see [`Active::find_user_at`](super::service::Active::find_user_at).

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::AuthError;
use rustryx_core::identity::{IdpProvider, Principal, UserSource};
use rustryx_core::ports::{ExternalGroups, GroupResolver};
use time::OffsetDateTime;
use tokio::time::timeout;

use super::cache::CachedGroups;
use super::error::LdapError;
use super::groups::{self, WalkOutcome};
use super::pool::PooledConnection;
use super::service::{Active, LdapService};
use super::stats::Counter;
use super::user::{DirectoryUser, UserLookup};

/// How a group set was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupFreshness {
    /// Searched in the directory just now.
    Fresh,
    /// Served from the cache, younger than the cache TTL.
    Cached,
    /// `groups_stale_on_error` only: the directory could not be reached and this is the
    /// last confirmed set, at most five minutes old. `confirmed_at` is NOT "now".
    StaleWithinWindow,
}

/// Groups together with the time the directory confirmed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGroups {
    /// DNs as the directory returned them, unmodified.
    pub groups: Vec<String>,
    /// When the directory last confirmed this set. Use it for
    /// `Principal::groups_confirmed_at`; never substitute the current time.
    pub confirmed_at: OffsetDateTime,
    /// How the set was obtained.
    pub freshness: GroupFreshness,
}

/// Why one attempt to ask the directory failed.
enum GroupFailure {
    /// The user is not (uniquely) in the directory any more.
    UserGone,
    /// Anything else; the text is for logs.
    Failed(String),
}

impl Active {
    /// Applies the policy described in the module documentation.
    ///
    /// `known` is the user entry when the caller has just found it (login), which
    /// saves one search. `dn_hint` is the DN of the user when it is known from earlier
    /// (`Principal::external_id`), which turns the subtree search into a read of that entry.
    pub(crate) async fn groups_for_user(
        &self,
        username: &str,
        known: Option<&DirectoryUser>,
        dn_hint: Option<&str>,
    ) -> Result<ResolvedGroups, AuthError> {
        if let Some(entry) = self.cache.fresh(username, self.clock.now()).await {
            return Ok(resolved(entry, GroupFreshness::Cached));
        }
        let hint = match dn_hint {
            Some(dn) => Some(dn.to_owned()),
            None => self.cache.known_dn(username).await,
        };
        let attempt = timeout(
            self.settings.timeouts.group_deadline,
            self.search_groups(username, known, hint.as_deref()),
        )
        .await;
        match attempt {
            Ok(Ok((groups, user_dn))) => {
                let entry = CachedGroups {
                    groups,
                    confirmed_at: self.clock.now(),
                    user_dn: Some(user_dn),
                };
                self.cache.put(username, entry.clone()).await;
                Ok(resolved(entry, GroupFreshness::Fresh))
            }
            Ok(Err(GroupFailure::UserGone)) => {
                self.cache.remove(username).await;
                tracing::warn!(user = %username, "ldap user is gone from the directory; group lookup refused");
                Err(AuthError::GroupLookupFailed(
                    "user is no longer in the directory".to_owned(),
                ))
            }
            Ok(Err(GroupFailure::Failed(reason))) => {
                self.fall_back(username, &reason, "ldap group lookup failed")
                    .await
            }
            Err(_) => {
                self.fall_back(
                    username,
                    "group deadline exceeded",
                    "ldap group lookup timed out",
                )
                .await
            }
        }
    }

    /// The groups of the user and the DN of the entry they were read for.
    async fn search_groups(
        &self,
        username: &str,
        known: Option<&DirectoryUser>,
        dn_hint: Option<&str>,
    ) -> Result<(Vec<String>, String), GroupFailure> {
        let mut conn = self
            .pool
            .checkout()
            .await
            .map_err(|error| GroupFailure::Failed(error.to_string()))?;
        let found;
        let user = if let Some(user) = known {
            user
        } else {
            found = self.locate_user(&mut conn, username, dn_hint).await?;
            &found
        };
        let outcome = groups::walk(&mut conn, &self.settings.groups, user)
            .await
            .map_err(|error| GroupFailure::Failed(error.to_string()))?;
        self.report_limits(&outcome, username);
        Ok((outcome.groups, user.dn.clone()))
    }

    /// Finds the user for a refresh: the entry at the remembered DN when there is one, the
    /// subtree search when there is none or when the hint no longer leads to the user (the
    /// entry moved, or the account is gone or disabled, and then the subtree search says so).
    async fn locate_user(
        &self,
        conn: &mut PooledConnection,
        username: &str,
        dn_hint: Option<&str>,
    ) -> Result<DirectoryUser, GroupFailure> {
        let failed = |error: LdapError| GroupFailure::Failed(error.to_string());
        if let Some(dn) = dn_hint
            && let UserLookup::Found(user) = self
                .find_user_at(conn, dn, username)
                .await
                .map_err(failed)?
        {
            return Ok(user);
        }
        match self.find_user(conn, username).await.map_err(failed)? {
            UserLookup::Found(user) => Ok(user),
            UserLookup::NotFound | UserLookup::Ambiguous => Err(GroupFailure::UserGone),
        }
    }

    fn report_limits(&self, outcome: &WalkOutcome, username: &str) {
        if outcome.depth_limit_hit {
            self.stats.bump(Counter::GroupDepthLimit);
            tracing::warn!(
                user = %username,
                max_depth = self.settings.groups.max_depth,
                "ldap group walk stopped at the depth limit; deeper groups are not granted"
            );
        }
        if outcome.size_limit_hit {
            self.stats.bump(Counter::GroupSizeLimit);
            tracing::warn!(
                user = %username,
                max_groups = self.settings.groups.max_groups,
                "ldap group walk stopped at the group count limit; further groups are not granted"
            );
        }
    }

    /// The directory could not answer: fail, or in stale mode serve the last confirmed set
    /// while it is at most five minutes old.
    async fn fall_back(
        &self,
        username: &str,
        reason: &str,
        message: &'static str,
    ) -> Result<ResolvedGroups, AuthError> {
        self.stats.bump(Counter::GroupLookupFailure);
        if !self.settings.groups.stale_on_error {
            tracing::warn!(user = %username, %reason, "ldap group lookup failed; failing closed");
            return Err(AuthError::GroupLookupFailed(message.to_owned()));
        }
        if let Some(entry) = self.cache.within_window(username, self.clock.now()).await {
            self.stats.bump(Counter::GroupsServedStale);
            tracing::warn!(
                user = %username,
                %reason,
                confirmed_at = %entry.confirmed_at,
                "ldap group lookup failed; serving the last confirmed set (at most 5 minutes old)"
            );
            return Ok(resolved(entry, GroupFreshness::StaleWithinWindow));
        }
        self.stats.bump(Counter::GroupsExpired);
        tracing::warn!(
            user = %username,
            %reason,
            "ldap group lookup failed and no set confirmed within 5 minutes exists; failing closed"
        );
        Err(AuthError::GroupLookupFailed(message.to_owned()))
    }
}

fn resolved(entry: CachedGroups, freshness: GroupFreshness) -> ResolvedGroups {
    ResolvedGroups {
        groups: entry.groups,
        confirmed_at: entry.confirmed_at,
        freshness,
    }
}

/// Resolves LDAP groups of principals authenticated by [`LdapAuthenticator`](super::LdapAuthenticator).
///
/// `ExternalGroups::groups` holds the DNs exactly as the directory returned them. The
/// role mapper compares them for exact equality with `auth.ldap.admin_groups`, so no
/// case folding, trimming or reordering happens here.
#[derive(Debug)]
pub struct LdapGroupResolver {
    service: Arc<LdapService>,
}

impl LdapGroupResolver {
    /// Creates a resolver on the shared service.
    #[must_use]
    pub const fn new(service: Arc<LdapService>) -> Self {
        Self { service }
    }

    /// Same as [`GroupResolver::external_groups`] but also reports when the directory
    /// confirmed the set and whether it was served stale. `Ok(None)` means the
    /// principal is not an LDAP principal and this resolver has nothing to say.
    ///
    /// Cancel safe: an interrupted lookup changes nothing except possibly refreshing
    /// the cache, and a half finished walk is never cached.
    pub async fn resolve_detailed(
        &self,
        principal: &Principal,
    ) -> Result<Option<ResolvedGroups>, AuthError> {
        if principal.source != UserSource::Ldap {
            return Ok(None);
        }
        let Some(active) = self.service.active.as_ref() else {
            return Err(AuthError::GroupLookupFailed(
                "ldap is not enabled".to_owned(),
            ));
        };
        active
            .groups_for_user(&principal.username, None, principal.external_id.as_deref())
            .await
            .map(Some)
    }
}

#[async_trait]
impl GroupResolver for LdapGroupResolver {
    /// Cancel safe.
    ///
    /// Returns the groups of an LDAP principal. A lookup that fails and has no set
    /// confirmed in the last five minutes to fall back on is
    /// `Err(AuthError::GroupLookupFailed)`, never an empty list. A principal of another
    /// source has no LDAP groups: an empty list is the correct, successful answer.
    async fn external_groups(&self, principal: &Principal) -> Result<ExternalGroups, AuthError> {
        // `confirmed_at` is the time of the DIRECTORY's answer, also for a set served from the
        // cache: the chain times its five minute window from it, never from the time it
        // received the answer. For a principal that is not an LDAP one the empty list is
        // true as of now.
        let (groups, confirmed_at) = match self.resolve_detailed(principal).await? {
            Some(resolved) => (resolved.groups, resolved.confirmed_at),
            None => (Vec::new(), self.service.clock.now()),
        };
        Ok(ExternalGroups::new(IdpProvider::Ldap, groups, confirmed_at))
    }
}
