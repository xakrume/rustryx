//! Circuit breaker for one upstream host (SRE-25).
//!
//! State machine: `Closed` counts outcomes in a sliding window. When the window
//! holds at least `min_requests` outcomes and more than `failure_percent` of them
//! failed, the breaker opens for `open_for`. After that exactly one probe request
//! is admitted (`HalfOpen`); its outcome closes or reopens the breaker.
//!
//! Time is passed in by the caller (`tokio::time::Instant`), which keeps the state
//! machine deterministic under test and under `tokio::time::pause`.
//!
//! Cancel safety: a [`BreakerPermit`] that is dropped without a verdict (the request
//! future was cancelled) frees the probe slot instead of leaving the breaker stuck
//! in `HalfOpen` forever.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use crate::metrics;

/// Tuning of a [`CircuitBreaker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerConfig {
    /// Length of the sliding window of outcomes.
    pub window: Duration,
    /// Outcomes required in the window before the breaker may open.
    pub min_requests: usize,
    /// Open when failures exceed this percentage of the window.
    pub failure_percent: usize,
    /// How long the breaker stays open before admitting a probe.
    pub open_for: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(30),
            min_requests: 20,
            failure_percent: 50,
            open_for: Duration::from_secs(30),
        }
    }
}

/// Externally visible state, also exported as the `upstream_circuit_state` gauge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Requests flow.
    Closed,
    /// One probe is allowed or in flight.
    HalfOpen,
    /// Requests are refused.
    Open,
}

impl BreakerState {
    const fn gauge(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::HalfOpen => 1,
            Self::Open => 2,
        }
    }
}

enum Phase {
    Closed,
    Open { until: Instant },
    HalfOpen { probing: bool },
}

struct Inner {
    phase: Phase,
    window: VecDeque<(Instant, bool)>,
}

/// Breaker for one upstream host.
pub struct CircuitBreaker {
    config: BreakerConfig,
    label: String,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    /// Creates a closed breaker. `label` is the repository name used for the gauge.
    #[must_use]
    pub fn new(config: BreakerConfig, label: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            config,
            label: label.into(),
            inner: Mutex::new(Inner {
                phase: Phase::Closed,
                window: VecDeque::new(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The critical sections below never panic, but a poisoned lock must not
        // take the whole upstream path down with it.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> BreakerState {
        match self.lock().phase {
            Phase::Closed => BreakerState::Closed,
            Phase::Open { .. } => BreakerState::Open,
            Phase::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }

    /// Asks to send a request. `None` means the breaker is open (or a probe is in flight).
    ///
    /// The lock is held only for the state transition, never across an await.
    #[must_use]
    pub fn admit(self: &Arc<Self>, now: Instant) -> Option<BreakerPermit> {
        let mut inner = self.lock();
        let probe = match inner.phase {
            Phase::Closed => false,
            Phase::Open { until } if now >= until => {
                inner.phase = Phase::HalfOpen { probing: true };
                self.export(BreakerState::HalfOpen);
                true
            }
            Phase::HalfOpen { probing: false } => {
                inner.phase = Phase::HalfOpen { probing: true };
                true
            }
            Phase::Open { .. } | Phase::HalfOpen { probing: true } => return None,
        };
        drop(inner);
        Some(BreakerPermit {
            breaker: Arc::clone(self),
            probe,
            decided: false,
        })
    }

    fn record(&self, now: Instant, ok: bool, probe: bool) {
        let mut inner = self.lock();
        if probe {
            if ok {
                inner.phase = Phase::Closed;
                inner.window.clear();
                self.export(BreakerState::Closed);
            } else {
                inner.phase = Phase::Open {
                    until: now + self.config.open_for,
                };
                self.export(BreakerState::Open);
            }
            return;
        }
        // Outcomes of requests admitted before the breaker opened are stale: only
        // the closed state keeps a window.
        if !matches!(inner.phase, Phase::Closed) {
            return;
        }
        inner.window.push_back((now, ok));
        while let Some(&(at, _)) = inner.window.front() {
            if now.saturating_duration_since(at) > self.config.window {
                inner.window.pop_front();
            } else {
                break;
            }
        }
        let total = inner.window.len();
        let failures = inner.window.iter().filter(|(_, ok)| !ok).count();
        if total >= self.config.min_requests && failures * 100 > total * self.config.failure_percent
        {
            inner.phase = Phase::Open {
                until: now + self.config.open_for,
            };
            inner.window.clear();
            self.export(BreakerState::Open);
        }
    }

    fn abandon(&self, probe: bool) {
        if !probe {
            return;
        }
        let mut inner = self.lock();
        if matches!(inner.phase, Phase::HalfOpen { probing: true }) {
            inner.phase = Phase::HalfOpen { probing: false };
        }
    }

    fn export(&self, state: BreakerState) {
        metrics::circuit_state(&self.label, state.gauge());
    }
}

/// Permission to send one request through a breaker. Report the outcome with
/// [`BreakerPermit::record`]; dropping it without one is treated as "no information".
pub struct BreakerPermit {
    breaker: Arc<CircuitBreaker>,
    probe: bool,
    decided: bool,
}

impl BreakerPermit {
    /// True when this request is the half-open probe.
    #[must_use]
    pub fn is_probe(&self) -> bool {
        self.probe
    }

    /// Reports the outcome: `ok` for an upstream that is alive (any non-5xx answer),
    /// `false` for timeouts, connection failures and 5xx.
    pub fn record(mut self, now: Instant, ok: bool) {
        self.decided = true;
        self.breaker.record(now, ok, self.probe);
    }
}

impl Drop for BreakerPermit {
    fn drop(&mut self) {
        if !self.decided {
            self.breaker.abandon(self.probe);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BreakerConfig {
        BreakerConfig {
            window: Duration::from_secs(30),
            min_requests: 4,
            failure_percent: 50,
            open_for: Duration::from_secs(30),
        }
    }

    fn feed(breaker: &Arc<CircuitBreaker>, now: Instant, outcomes: &[bool]) {
        for ok in outcomes {
            if let Some(permit) = breaker.admit(now) {
                permit.record(now, *ok);
            }
        }
    }

    #[tokio::test]
    async fn stays_closed_below_threshold_or_sample_size() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[false, false, false]);
        assert_eq!(
            b.state(),
            BreakerState::Closed,
            "3 outcomes are below min_requests"
        );
        let b = CircuitBreaker::new(config(), "r");
        feed(&b, now, &[true, true, false, false]);
        assert_eq!(
            b.state(),
            BreakerState::Closed,
            "exactly 50% does not exceed 50%"
        );
    }

    #[tokio::test]
    async fn opens_when_failures_exceed_the_ratio() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[true, false, false, false]);
        assert_eq!(b.state(), BreakerState::Open);
        assert!(
            b.admit(now).is_none(),
            "an open breaker sends nothing upstream"
        );
    }

    #[tokio::test]
    async fn must_not_send_requests_while_open_and_probe_after_the_pause() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[false; 4]);
        assert!(b.admit(now + Duration::from_secs(29)).is_none());
        let probe = b
            .admit(now + Duration::from_secs(30))
            .expect("probe after open_for");
        assert!(probe.is_probe());
        assert_eq!(b.state(), BreakerState::HalfOpen);
        assert!(
            b.admit(now + Duration::from_secs(31)).is_none(),
            "only one probe at a time"
        );
        probe.record(now + Duration::from_secs(31), true);
        assert_eq!(b.state(), BreakerState::Closed);
        assert!(b.admit(now + Duration::from_secs(32)).is_some());
    }

    #[tokio::test]
    async fn failed_probe_reopens_for_another_period() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[false; 4]);
        let t1 = now + Duration::from_secs(30);
        b.admit(t1).unwrap().record(t1, false);
        assert_eq!(b.state(), BreakerState::Open);
        assert!(b.admit(t1 + Duration::from_secs(29)).is_none());
        assert!(b.admit(t1 + Duration::from_secs(30)).is_some());
    }

    #[tokio::test]
    async fn must_not_stay_half_open_when_the_probe_is_cancelled() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[false; 4]);
        let t1 = now + Duration::from_secs(30);
        let probe = b.admit(t1).unwrap();
        drop(probe); // the request future was cancelled before a verdict
        assert_eq!(b.state(), BreakerState::HalfOpen);
        assert!(b.admit(t1).is_some(), "the probe slot must be free again");
    }

    #[tokio::test]
    async fn old_outcomes_fall_out_of_the_window() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        feed(&b, now, &[false, false, false]);
        // 31 s later the three failures are outside the window; one more must not open it.
        let later = now + Duration::from_secs(31);
        feed(&b, later, &[false]);
        assert_eq!(b.state(), BreakerState::Closed);
    }

    #[tokio::test]
    async fn stale_outcomes_do_not_matter_while_open() {
        let b = CircuitBreaker::new(config(), "r");
        let now = Instant::now();
        let in_flight = b.admit(now).unwrap();
        feed(&b, now, &[false; 4]);
        assert_eq!(b.state(), BreakerState::Open);
        in_flight.record(now, true);
        assert_eq!(
            b.state(),
            BreakerState::Open,
            "a late success must not close an open breaker"
        );
    }

    #[tokio::test]
    async fn default_config_matches_the_normative_values() {
        let c = BreakerConfig::default();
        assert_eq!(c.window, Duration::from_secs(30));
        assert_eq!(c.min_requests, 20);
        assert_eq!(c.failure_percent, 50);
        assert_eq!(c.open_for, Duration::from_secs(30));
    }
}
