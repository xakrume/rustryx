//! Revoking the token a reissue replaced, after a short grace.
//!
//! A reissue gives the browser a new token and leaves the old one valid until its own end. A
//! copy of the old cookie (a leaked log line, a stolen device image) would then work for as long
//! as the session lives, although the browser has long moved on. So the old token id is revoked
//! once the new one is out.
//!
//! **Why not at once.** A single page sends many requests in parallel. When the response of the
//! first one carries the new cookie, the others are already in flight with the old one, and a
//! revocation that took effect at once would answer them 401 for a token the server itself had
//! just replaced. The revocation therefore waits `SessionSettings::revoke_grace` (30 seconds by
//! default), which is long enough for in-flight requests and short enough that a stolen copy is
//! worth very little.
//!
//! **How it is waited for.** Each revocation is a task on a `TaskTracker` owned by the manager
//! (never a detached spawn), sleeping for the grace OR until the manager is drained. A drain
//! cancels the sleeps, so at shutdown the pending revocations run at once instead of being lost
//! with the process, and then waits for them, bounded by a deadline. A crash still loses the
//! revocations that were waiting; the old tokens then live to their own end, as they did before
//! this existed, which is the accepted price of not writing a revocation that is not due yet.
//!
//! **The family link.** A token replaced a moment ago is still valid, so a logout on its
//! successor must end it too, not leave it to the timer. Each retired token is remembered under
//! the id of its successor, and a logout takes the whole chain (a successor of a successor
//! included). The link lives in memory of the node that issued the successor: a logout that lands
//! on another node still ends the old token when this node's timer fires, so the window is the
//! grace, never longer.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustryx_core::ids::UserId;
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::ports::SessionIssuer;

/// A token that a reissue replaced and that still has to be revoked.
#[derive(Debug, Clone)]
pub(crate) struct Stale {
    pub user_id: UserId,
    pub tid: String,
    /// The `exp` of the token: the revocation row is useless after it.
    pub until: OffsetDateTime,
}

#[derive(Default)]
struct Links {
    /// Retired tokens by the id of the token that replaced them.
    previous: HashMap<String, Vec<Stale>>,
    /// Ids with a revocation task already waiting: parallel requests that reissue from the same
    /// old token must not start one task each.
    scheduled: HashSet<String>,
}

fn lock(links: &Mutex<Links>) -> MutexGuard<'_, Links> {
    links.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Drops the bookkeeping of a token whose revocation ran.
fn forget(links: &Mutex<Links>, tid: &str) {
    let mut links = lock(links);
    links.scheduled.remove(tid);
    links.previous.retain(|_, olds| {
        olds.retain(|old| old.tid != tid);
        !olds.is_empty()
    });
}

/// Revokes one token; a failure is logged and never reaches a request: the user's request
/// already succeeded, and the token still ends at its own `exp`.
async fn revoke_logged(issuer: &dyn SessionIssuer, stale: &Stale) {
    if let Err(error) = issuer.revoke(stale.user_id, &stale.tid, stale.until).await {
        tracing::warn!(%error, "could not revoke a replaced session token");
    }
}

async fn revoke_after_grace(
    issuer: Arc<dyn SessionIssuer>,
    links: Arc<Mutex<Links>>,
    cancel: CancellationToken,
    due: Instant,
    stale: Stale,
) {
    tokio::select! {
        () = cancel.cancelled() => {}
        () = tokio::time::sleep_until(due) => {}
    }
    revoke_logged(issuer.as_ref(), &stale).await;
    forget(&links, &stale.tid);
}

/// The pending revocations of one session manager.
pub(crate) struct Revocations {
    issuer: Arc<dyn SessionIssuer>,
    grace: Duration,
    tracker: TaskTracker,
    cancel: CancellationToken,
    links: Arc<Mutex<Links>>,
}

impl Revocations {
    pub(crate) fn new(issuer: Arc<dyn SessionIssuer>, grace: Duration) -> Self {
        Self {
            issuer,
            grace,
            tracker: TaskTracker::new(),
            cancel: CancellationToken::new(),
            links: Arc::new(Mutex::new(Links::default())),
        }
    }

    /// Schedules the revocation of `stale`, replaced by `successor`.
    ///
    /// A zero grace, or no tokio runtime to wait on (a unit test driving the manager with a bare
    /// executor), revokes before returning: there is nothing to wait with, and an immediate
    /// revocation is the safe side of the two.
    pub(crate) async fn retire(&self, stale: Stale, successor: &str) {
        let start_task = {
            let mut links = lock(&self.links);
            links
                .previous
                .entry(successor.to_owned())
                .or_default()
                .push(stale.clone());
            links.scheduled.insert(stale.tid.clone())
        };
        if !start_task {
            return;
        }
        let handle = tokio::runtime::Handle::try_current()
            .ok()
            .filter(|_| !self.grace.is_zero());
        if let Some(handle) = handle {
            drop(self.tracker.spawn_on(
                revoke_after_grace(
                    Arc::clone(&self.issuer),
                    Arc::clone(&self.links),
                    self.cancel.clone(),
                    // Counted from now, not from whenever the runtime first polls the task.
                    Instant::now() + self.grace,
                    stale,
                ),
                &handle,
            ));
        } else {
            revoke_logged(self.issuer.as_ref(), &stale).await;
            forget(&self.links, &stale.tid);
        }
    }

    /// Revokes at once every token the chain of `tid` replaced and that is still waiting.
    pub(crate) async fn revoke_predecessors_of(&self, tid: &str) {
        let chain = {
            let mut links = lock(&self.links);
            let mut found = Vec::new();
            let mut queue = vec![tid.to_owned()];
            while let Some(next) = queue.pop() {
                for old in links.previous.remove(&next).unwrap_or_default() {
                    queue.push(old.tid.clone());
                    found.push(old);
                }
            }
            found
        };
        for old in chain {
            revoke_logged(self.issuer.as_ref(), &old).await;
        }
    }

    /// Runs the waiting revocations now and waits for them, for at most `deadline`. Returns how
    /// many did not finish.
    pub(crate) async fn drain(&self, deadline: Duration) -> usize {
        self.cancel.cancel();
        self.tracker.close();
        if tokio::time::timeout(deadline, self.tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                pending = self.tracker.len(),
                "session revocations did not finish before the shutdown deadline"
            );
        }
        self.tracker.len()
    }

    /// Revocations waiting or running.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.tracker.len()
    }
}
