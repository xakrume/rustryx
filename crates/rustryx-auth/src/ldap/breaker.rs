//! Circuit breaker in front of the creation of service connections (SEC-08).
//!
//! When the directory is down every login would otherwise wait out a full connect timeout,
//! and a service account with a wrong password would be retried on every login until the
//! directory locks it. After `BREAKER_FAILURES` failures in a row the breaker opens and
//! refuses at once for `BREAKER_COOLDOWN`; then it lets exactly ONE attempt through (the
//! trial). A trial that succeeds closes the breaker, one that fails (or is cancelled before
//! it finished: the ticket counts an unfinished attempt as a failure) opens it again.
//!
//! Only the creation of a service connection passes through it. Idle healthy connections
//! keep working while it is open: they are proof the directory answers.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

/// Consecutive failures that open the breaker.
pub(crate) const BREAKER_FAILURES: u32 = 5;
/// How long an open breaker refuses before it allows a trial.
pub(crate) const BREAKER_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
enum State {
    Closed {
        failures: u32,
    },
    Open {
        until: Instant,
    },
    /// One trial is in flight; everybody else is refused until it ends.
    Trial,
}

#[derive(Debug)]
pub(crate) struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    state: Mutex<State>,
}

/// Permission to make one attempt. Dropping it without calling [`Ticket::succeed`] or
/// [`Ticket::fail`] counts as a failure: the attempt was cancelled and nobody knows how
/// it would have ended.
#[derive(Debug)]
#[must_use = "an attempt must be reported with succeed() or fail()"]
pub(crate) struct Ticket<'a> {
    breaker: &'a CircuitBreaker,
    resolved: bool,
}

impl CircuitBreaker {
    pub(crate) fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            threshold,
            cooldown,
            state: Mutex::new(State::Closed { failures: 0 }),
        }
    }

    /// `None` when the attempt must be refused.
    pub(crate) fn admit(&self) -> Option<Ticket<'_>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let admitted = match *state {
            State::Closed { .. } => true,
            State::Open { until } if Instant::now() >= until => {
                *state = State::Trial;
                true
            }
            State::Open { .. } | State::Trial => false,
        };
        drop(state);
        // Lazily: a ticket that is built and dropped counts as a failed attempt, which
        // must never happen to a refusal (and would lock this mutex a second time).
        admitted.then(|| Ticket {
            breaker: self,
            resolved: false,
        })
    }

    fn record_success(&self) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = State::Closed { failures: 0 };
    }

    /// Returns true when this failure opened the breaker.
    fn record_failure(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (next, tripped) = match *state {
            State::Closed { failures } if failures + 1 < self.threshold => (
                State::Closed {
                    failures: failures + 1,
                },
                false,
            ),
            State::Closed { .. } | State::Trial | State::Open { .. } => (
                State::Open {
                    until: Instant::now() + self.cooldown,
                },
                true,
            ),
        };
        *state = next;
        tripped
    }
}

impl Ticket<'_> {
    pub(crate) fn succeed(mut self) {
        self.resolved = true;
        self.breaker.record_success();
    }

    /// Returns true when this failure opened the breaker.
    pub(crate) fn fail(mut self) -> bool {
        self.resolved = true;
        self.breaker.record_failure()
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.breaker.record_failure();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker() -> CircuitBreaker {
        CircuitBreaker::new(3, Duration::from_secs(10))
    }

    fn fail_once(breaker: &CircuitBreaker) -> bool {
        breaker.admit().expect("admitted").fail()
    }

    #[tokio::test(start_paused = true)]
    async fn opens_after_the_threshold_of_consecutive_failures() {
        let breaker = breaker();
        assert!(!fail_once(&breaker));
        assert!(!fail_once(&breaker));
        assert!(fail_once(&breaker), "the third failure opens it");
        assert!(breaker.admit().is_none(), "open: refused at once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_resets_the_count() {
        let breaker = breaker();
        fail_once(&breaker);
        fail_once(&breaker);
        breaker.admit().unwrap().succeed();
        fail_once(&breaker);
        fail_once(&breaker);
        assert!(
            breaker.admit().is_some(),
            "two failures after a success do not open it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lets_exactly_one_trial_through_after_the_cooldown() {
        let breaker = breaker();
        for _ in 0..3 {
            fail_once(&breaker);
        }
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(breaker.admit().is_none(), "still cooling down");
        tokio::time::advance(Duration::from_secs(1)).await;
        let trial = breaker.admit();
        assert!(trial.is_some(), "the trial");
        assert!(
            breaker.admit().is_none(),
            "nobody else while the trial runs"
        );
        trial.unwrap().succeed();
        assert!(breaker.admit().is_some(), "a good trial closes the breaker");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_trial_opens_it_again_for_a_full_cooldown() {
        let breaker = breaker();
        for _ in 0..3 {
            fail_once(&breaker);
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(breaker.admit().unwrap().fail());
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(breaker.admit().is_none());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(breaker.admit().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_leave_the_breaker_stuck_when_the_trial_is_cancelled() {
        let breaker = breaker();
        for _ in 0..3 {
            fail_once(&breaker);
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        let trial = breaker.admit();
        assert!(trial.is_some());
        drop(trial);
        assert!(
            breaker.admit().is_none(),
            "an abandoned trial counts as a failure"
        );
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            breaker.admit().is_some(),
            "and the next cooldown ends normally"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_not_an_attempt_and_does_not_extend_the_cooldown() {
        let breaker = breaker();
        for _ in 0..3 {
            fail_once(&breaker);
        }
        for _ in 0..50 {
            assert!(breaker.admit().is_none());
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        // 5 s passed, 5 s are left: refusals did not push the end of the cooldown.
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(breaker.admit().is_some());
    }
}
