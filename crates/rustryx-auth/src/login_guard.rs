//! Brute-force protection for password logins (local accounts and LDAP).
//!
//! Failures are counted through `LoginAttemptRepo`, which PostgreSQL implements:
//! the counter is shared by every node and every worker thread. A per-process
//! counter would let an attacker multiply their budget by the number of replicas,
//! so this module keeps no counter of its own.
//!
//! Two counters per attempt:
//!
//! - `(account, ip)`: a single source hammering one account;
//! - `(account)`: the same account attacked from many addresses.
//!
//! After [`GuardPolicy::threshold`] failures a counter locks for a delay that
//! doubles with every further failure, from 1 minute up to 30 minutes. The
//! account is whatever name was typed, whether or not it exists, so a lock reveals
//! nothing about which accounts are real.
//!
//! Fail closed: when the repository is unreachable no password login proceeds
//! ([`AuthError::GuardUnavailable`]). Allowing logins while the counters are
//! blind would turn a database outage into an unthrottled guessing window.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError::{LockedOut, GuardUnavailable}`, documented on each method

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::error::AuthError;
use rustryx_core::ports::{Clock, LoginAttemptRepo, LoginKey, LoginState};
use time::OffsetDateTime;

/// Failures after which a counter starts to lock (SEC-10).
pub const FAILURE_THRESHOLD: u32 = 5;
/// Delay after the first locking failure.
pub const MIN_LOCK_DELAY: Duration = Duration::from_mins(1);
/// Longest delay.
pub const MAX_LOCK_DELAY: Duration = Duration::from_mins(30);
/// Longest account name that is counted; longer names share their prefix's counter.
const MAX_ACCOUNT_CHARS: usize = 256;

/// Case folds and bounds an account name for use as a counter key.
///
/// Folding matters because LDAP and Active Directory compare names without case:
/// `ALICE` and `alice` are one account there, and an attacker who could vary the
/// case would otherwise get a fresh counter for every spelling.
#[must_use]
pub fn fold_account(account: &str) -> String {
    account
        .trim()
        .to_lowercase()
        .chars()
        .take(MAX_ACCOUNT_CHARS)
        .collect()
}

/// Lock schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardPolicy {
    /// Failures before the first lock.
    pub threshold: u32,
    /// Delay at the threshold.
    pub base_delay: Duration,
    /// Delay cap.
    pub max_delay: Duration,
}

impl Default for GuardPolicy {
    fn default() -> Self {
        Self {
            threshold: FAILURE_THRESHOLD,
            base_delay: MIN_LOCK_DELAY,
            max_delay: MAX_LOCK_DELAY,
        }
    }
}

impl GuardPolicy {
    /// Lock delay after `failures` consecutive failures, `None` below the threshold.
    ///
    /// `base * 2^(failures - threshold)`, capped at `max_delay`.
    #[must_use]
    pub fn delay_for(&self, failures: u32) -> Option<Duration> {
        let excess = failures.checked_sub(self.threshold)?;
        let factor = 1_u32.checked_shl(excess).unwrap_or(u32::MAX);
        Some(self.base_delay.saturating_mul(factor).min(self.max_delay))
    }
}

/// What `check` saw, handed back to `record_success` so a clean login costs no write.
#[derive(Debug, Clone)]
pub struct GuardTicket {
    keys: CounterKeys,
    had_failures: bool,
}

/// The counters one attempt touches. Without a peer address there is no pair counter:
/// using the account key twice would count every failure twice and lock accounts
/// after three attempts instead of five.
#[derive(Debug, Clone)]
struct CounterKeys {
    pair: Option<LoginKey>,
    account: LoginKey,
}

/// Failed-login guard over a shared repository.
pub struct LoginGuard {
    repo: Arc<dyn LoginAttemptRepo>,
    clock: Arc<dyn Clock>,
    policy: GuardPolicy,
}

impl std::fmt::Debug for LoginGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginGuard")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl LoginGuard {
    /// Builds a guard.
    #[must_use]
    pub fn new(
        repo: Arc<dyn LoginAttemptRepo>,
        clock: Arc<dyn Clock>,
        policy: GuardPolicy,
    ) -> Self {
        Self {
            repo,
            clock,
            policy,
        }
    }

    fn keys(account: &str, ip: Option<IpAddr>) -> CounterKeys {
        let account = fold_account(account);
        let pair = ip.map(|address| LoginKey {
            account: account.clone(),
            // `::ffff:1.2.3.4` and `1.2.3.4` are one peer: without canonicalising, a
            // dual-stack listener would give an attacker two counters.
            ip: Some(address.to_canonical()),
        });
        CounterKeys {
            pair,
            account: LoginKey { account, ip: None },
        }
    }

    async fn read_state(&self, key: Option<&LoginKey>) -> Result<LoginState, StoreError> {
        match key {
            Some(key) => self.repo.state(key).await,
            None => Ok(LoginState {
                failures: 0,
                last_failure_at: None,
            }),
        }
    }

    fn unavailable(operation: &'static str, error: &StoreError) -> AuthError {
        tracing::error!(operation, %error, "login attempt store failed, refusing password login");
        AuthError::GuardUnavailable
    }

    /// Remaining lock of one counter, zero when open.
    fn remaining(&self, state: &LoginState, now: OffsetDateTime) -> Duration {
        let Some(delay) = self.policy.delay_for(state.failures) else {
            return Duration::ZERO;
        };
        // A counter past the threshold without a timestamp is a repository bug.
        // Treating it as "failed just now" keeps the account locked, which is the safe side.
        let last = state.last_failure_at.unwrap_or(now);
        let Ok(delay) = time::Duration::try_from(delay) else {
            return self.policy.max_delay;
        };
        let remaining = last.saturating_add(delay) - now;
        // A negative value means the lock has run out. A value above the policy maximum
        // can only come from a clock that runs ahead on another node: cap it so one
        // skewed node cannot lock an account for hours.
        Duration::try_from(remaining)
            .unwrap_or(Duration::ZERO)
            .min(self.policy.max_delay)
    }

    /// Refuses the attempt while either counter is locked.
    ///
    /// Call before verifying the password: a locked account must not be able to
    /// tell a right password from a wrong one.
    pub async fn check(&self, account: &str, ip: Option<IpAddr>) -> Result<GuardTicket, AuthError> {
        let keys = Self::keys(account, ip);
        let (by_pair, by_account) = futures::future::try_join(
            self.read_state(keys.pair.as_ref()),
            self.repo.state(&keys.account),
        )
        .await
        .map_err(|error| Self::unavailable("state", &error))?;
        let now = self.clock.now();
        let retry_after = self
            .remaining(&by_pair, now)
            .max(self.remaining(&by_account, now));
        if !retry_after.is_zero() {
            metrics::counter!("rustryx_auth_login_locked_total").increment(1);
            tracing::warn!(account = ?keys.account.account, ?retry_after, "login refused: locked out");
            return Err(AuthError::LockedOut { retry_after });
        }
        Ok(GuardTicket {
            had_failures: by_pair.failures > 0 || by_account.failures > 0,
            keys,
        })
    }

    /// Counts a failed attempt on both counters.
    pub async fn record_failure(&self, account: &str, ip: Option<IpAddr>) -> Result<(), AuthError> {
        let keys = Self::keys(account, ip);
        let now = self.clock.now();
        let pair = async {
            match &keys.pair {
                Some(key) => self.repo.register_failure(key, now).await.map(Some),
                None => Ok(None),
            }
        };
        let (by_pair, by_account) =
            futures::future::try_join(pair, self.repo.register_failure(&keys.account, now))
                .await
                .map_err(|error| Self::unavailable("register_failure", &error))?;
        metrics::counter!("rustryx_auth_login_failures_total").increment(1);
        let worst = by_pair
            .map_or(0, |state| state.failures)
            .max(by_account.failures);
        if self.policy.delay_for(worst).is_some() {
            tracing::warn!(account = ?keys.account.account, failures = worst, "account is now locked");
        }
        Ok(())
    }

    /// Clears both counters after a successful login. Free when nothing was counted.
    pub async fn record_success(&self, ticket: &GuardTicket) -> Result<(), AuthError> {
        if !ticket.had_failures {
            return Ok(());
        }
        let pair = async {
            match &ticket.keys.pair {
                Some(key) => self.repo.reset(key).await,
                None => Ok(()),
            }
        };
        futures::future::try_join(pair, self.repo.reset(&ticket.keys.account))
            .await
            .map_err(|error| Self::unavailable("reset", &error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use rustryx_testkit::{FixedClock, MemStore};

    use super::*;

    const IP_A: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const IP_B: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    fn guard_over(store: &MemStore, clock: &FixedClock) -> LoginGuard {
        LoginGuard::new(
            Arc::new(store.clone()),
            Arc::new(clock.clone()),
            GuardPolicy::default(),
        )
    }

    async fn fail_n(guard: &LoginGuard, account: &str, ip: Option<IpAddr>, n: u32) {
        for _ in 0..n {
            guard.record_failure(account, ip).await.unwrap();
        }
    }

    fn locked_for(result: &Result<GuardTicket, AuthError>) -> Duration {
        let Err(AuthError::LockedOut { retry_after }) = result else {
            unreachable!("expected a lock, got {result:?}");
        };
        *retry_after
    }

    #[test]
    fn schedule_doubles_from_one_to_thirty_minutes() {
        let policy = GuardPolicy::default();
        let minutes = |failures| policy.delay_for(failures).map(|d| d.as_secs() / 60);
        assert_eq!(minutes(0), None);
        assert_eq!(minutes(4), None);
        assert_eq!(minutes(5), Some(1));
        assert_eq!(minutes(6), Some(2));
        assert_eq!(minutes(7), Some(4));
        assert_eq!(minutes(8), Some(8));
        assert_eq!(minutes(9), Some(16));
        assert_eq!(minutes(10), Some(30));
        assert_eq!(minutes(50), Some(30));
        assert_eq!(minutes(u32::MAX), Some(30), "no overflow at the extreme");
    }

    #[test]
    fn account_names_are_case_folded_trimmed_and_bounded() {
        assert_eq!(fold_account("  Alice "), "alice");
        assert_eq!(fold_account("ALICE"), fold_account("alice"));
        assert_eq!(fold_account(&"x".repeat(1000)).len(), MAX_ACCOUNT_CHARS);
    }

    #[tokio::test]
    async fn opens_below_the_threshold_and_locks_at_it() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 4).await;
        guard.check("alice", Some(IP_A)).await.unwrap();
        guard.record_failure("alice", Some(IP_A)).await.unwrap();
        let wait = locked_for(&guard.check("alice", Some(IP_A)).await);
        assert_eq!(wait, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn lock_runs_out_with_time_and_shrinks_meanwhile() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 5).await;
        clock.advance(Duration::from_secs(20));
        assert_eq!(
            locked_for(&guard.check("alice", Some(IP_A)).await),
            Duration::from_secs(40)
        );
        clock.advance(Duration::from_secs(40));
        guard.check("alice", Some(IP_A)).await.unwrap();
    }

    #[tokio::test]
    async fn further_failures_lengthen_the_lock() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 7).await;
        assert_eq!(
            locked_for(&guard.check("alice", Some(IP_A)).await),
            Duration::from_mins(4)
        );
    }

    #[tokio::test]
    async fn must_not_keep_a_per_process_counter() {
        // Two guards over one repository stand for two nodes (or two worker threads).
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let node_a = guard_over(&store, &clock);
        let node_b = guard_over(&store, &clock);
        fail_n(&node_a, "alice", Some(IP_A), 3).await;
        fail_n(&node_b, "alice", Some(IP_A), 2).await;
        assert!(matches!(
            node_a.check("alice", Some(IP_A)).await,
            Err(AuthError::LockedOut { .. })
        ));
        assert!(matches!(
            node_b.check("alice", Some(IP_A)).await,
            Err(AuthError::LockedOut { .. })
        ));
    }

    #[tokio::test]
    async fn account_counter_locks_even_when_every_ip_stays_below_the_threshold() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        for last_octet in 0..5_u8 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, last_octet));
            guard.record_failure("alice", Some(ip)).await.unwrap();
        }
        let fresh_ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
        assert!(matches!(
            guard.check("alice", Some(fresh_ip)).await,
            Err(AuthError::LockedOut { .. })
        ));
    }

    #[tokio::test]
    async fn pair_counter_is_separate_from_other_accounts() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 5).await;
        guard.check("bob", Some(IP_A)).await.unwrap();
        // Same account from another address: only the account counter applies, and
        // it also reached 5 here, so it is locked as well.
        assert!(guard.check("alice", Some(IP_B)).await.is_err());
    }

    #[tokio::test]
    async fn must_not_let_case_changes_dodge_the_counter() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        for spelling in ["alice", "ALICE", "Alice", "aLiCe", " alice "] {
            guard.record_failure(spelling, Some(IP_A)).await.unwrap();
        }
        assert!(guard.check("ALICE", Some(IP_A)).await.is_err());
    }

    #[tokio::test]
    async fn ipv4_mapped_ipv6_counts_as_the_same_peer() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        fail_n(&guard, "alice", Some(mapped), 3).await;
        fail_n(&guard, "alice", Some(IP_A), 2).await;
        let key = LoginKey {
            account: "alice".into(),
            ip: Some(IP_A),
        };
        assert_eq!(store.state(&key).await.unwrap().failures, 5);
    }

    #[tokio::test]
    async fn missing_address_counts_each_failure_exactly_once() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", None, 4).await;
        let key = LoginKey {
            account: "alice".into(),
            ip: None,
        };
        assert_eq!(store.state(&key).await.unwrap().failures, 4);
        guard.check("alice", None).await.unwrap();
        guard.record_failure("alice", None).await.unwrap();
        assert!(guard.check("alice", None).await.is_err());
        assert!(guard.check("alice", Some(IP_B)).await.is_err());
        let ticket_clear = guard_over(&MemStore::new(), &clock);
        ticket_clear.check("bob", None).await.unwrap();
    }

    #[tokio::test]
    async fn success_resets_both_counters() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 3).await;
        let ticket = guard.check("alice", Some(IP_A)).await.unwrap();
        guard.record_success(&ticket).await.unwrap();
        fail_n(&guard, "alice", Some(IP_A), 4).await;
        guard.check("alice", Some(IP_A)).await.unwrap();
        for key in [
            LoginKey {
                account: "alice".into(),
                ip: Some(IP_A),
            },
            LoginKey {
                account: "alice".into(),
                ip: None,
            },
        ] {
            assert_eq!(store.state(&key).await.unwrap().failures, 4);
        }
    }

    /// Counts calls so a test can assert that a clean login performs no writes.
    struct CountingRepo {
        inner: MemStore,
        writes: AtomicUsize,
    }

    #[async_trait]
    impl LoginAttemptRepo for CountingRepo {
        async fn register_failure(
            &self,
            key: &LoginKey,
            at: OffsetDateTime,
        ) -> Result<LoginState, StoreError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.register_failure(key, at).await
        }
        async fn state(&self, key: &LoginKey) -> Result<LoginState, StoreError> {
            self.inner.state(key).await
        }
        async fn reset(&self, key: &LoginKey) -> Result<(), StoreError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.reset(key).await
        }
    }

    #[tokio::test]
    async fn a_clean_login_performs_no_counter_write() {
        let repo = Arc::new(CountingRepo {
            inner: MemStore::new(),
            writes: AtomicUsize::new(0),
        });
        let guard = LoginGuard::new(
            repo.clone(),
            Arc::new(FixedClock::default()),
            GuardPolicy::default(),
        );
        for _ in 0..10 {
            let ticket = guard.check("alice", Some(IP_A)).await.unwrap();
            guard.record_success(&ticket).await.unwrap();
        }
        assert_eq!(repo.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn must_fail_closed_when_the_counter_store_is_down() {
        let (store, clock) = (MemStore::new(), FixedClock::default());
        let guard = guard_over(&store, &clock);
        fail_n(&guard, "alice", Some(IP_A), 1).await;
        let ticket = guard.check("alice", Some(IP_A)).await.unwrap();
        store.set_unavailable(true);
        assert!(matches!(
            guard.check("alice", Some(IP_A)).await,
            Err(AuthError::GuardUnavailable)
        ));
        assert!(matches!(
            guard.record_failure("alice", Some(IP_A)).await,
            Err(AuthError::GuardUnavailable)
        ));
        assert!(matches!(
            guard.record_success(&ticket).await,
            Err(AuthError::GuardUnavailable)
        ));
        store.set_unavailable(false);
        guard.check("alice", Some(IP_A)).await.unwrap();
    }

    /// Returns a state past the threshold with no timestamp, or one dated far ahead.
    struct OddRepo(LoginState);

    #[async_trait]
    impl LoginAttemptRepo for OddRepo {
        async fn register_failure(
            &self,
            _key: &LoginKey,
            _at: OffsetDateTime,
        ) -> Result<LoginState, StoreError> {
            Ok(self.0.clone())
        }
        async fn state(&self, _key: &LoginKey) -> Result<LoginState, StoreError> {
            Ok(self.0.clone())
        }
        async fn reset(&self, _key: &LoginKey) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn must_stay_locked_when_the_timestamp_is_missing() {
        let guard = LoginGuard::new(
            Arc::new(OddRepo(LoginState {
                failures: 5,
                last_failure_at: None,
            })),
            Arc::new(FixedClock::default()),
            GuardPolicy::default(),
        );
        assert_eq!(
            locked_for(&guard.check("alice", None).await),
            Duration::from_secs(60)
        );
    }

    #[tokio::test]
    async fn must_cap_a_lock_dated_by_a_clock_that_runs_ahead() {
        let clock = FixedClock::default();
        let ahead = clock.now() + time::Duration::days(2);
        let guard = LoginGuard::new(
            Arc::new(OddRepo(LoginState {
                failures: 5,
                last_failure_at: Some(ahead),
            })),
            Arc::new(clock),
            GuardPolicy::default(),
        );
        assert_eq!(
            locked_for(&guard.check("alice", None).await),
            MAX_LOCK_DELAY
        );
    }

    #[tokio::test]
    async fn debug_output_names_the_type_and_policy() {
        let guard = guard_over(&MemStore::new(), &FixedClock::default());
        let ticket = guard.check("alice", Some(IP_A)).await.unwrap();
        assert!(format!("{guard:?}").contains("LoginGuard"));
        assert!(format!("{ticket:?}").contains("had_failures"));
    }
}
