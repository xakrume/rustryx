//! Admission control for blocking work (argon2id, bcrypt, other CPU-bound jobs).
//!
//! Two lanes, each a `tokio::sync::Semaphore`:
//!
//! - `password`: one permit per physical core. argon2id with the default
//!   parameters is memory-bound and saturates a core, so more concurrent hashes
//!   than cores only add latency and memory pressure (64 MiB each).
//! - `general`: 2x logical CPUs for everything else that has to leave the async
//!   workers.
//!
//! Invariants held by this module:
//!
//! 1. The permit is acquired BEFORE `spawn_blocking`. Spawning first and
//!    acquiring inside the closure would park blocking threads on the semaphore
//!    and let the (unbounded in practice) spawn queue grow under a login storm.
//! 2. The permit moves into the closure and is dropped when the work ends, not
//!    when the awaiting future ends. A cancelled caller leaves the blocking job
//!    running (it cannot be interrupted), so the lane must stay occupied until
//!    the CPU is actually free again.
//! 3. The number of waiters is bounded as well: past `max_waiting` the call
//!    fails immediately, and no waiter lives longer than `permit_wait`. The queue
//!    therefore never grows beyond a constant.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustryx_core::error::AuthError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::errmap::backend_failure;

/// Which lane a job runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateClass {
    /// Password hashing and verification.
    Password,
    /// Any other blocking job.
    General,
}

impl GateClass {
    const fn label(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::General => "general",
        }
    }
}

/// Size of one lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneLimits {
    /// Jobs running at the same time.
    pub permits: usize,
    /// Callers allowed to wait for a permit. Beyond that the call is rejected at once.
    pub max_waiting: usize,
}

/// Configuration of a [`BlockingGate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateConfig {
    /// Password lane.
    pub password: LaneLimits,
    /// General lane.
    pub general: LaneLimits,
    /// Longest wait for a permit. The HTTP layer's request deadline is the real bound;
    /// this value keeps the wait finite when the caller passes no deadline.
    pub permit_wait: Duration,
}

impl Default for GateConfig {
    /// Password lane = physical cores, general lane = 2x logical CPUs, waiters = permits.
    fn default() -> Self {
        let physical = num_cpus::get_physical().max(1);
        let general = num_cpus::get().max(1).saturating_mul(2);
        Self {
            password: LaneLimits {
                permits: physical,
                max_waiting: physical,
            },
            general: LaneLimits {
                permits: general,
                max_waiting: general,
            },
            permit_wait: Duration::from_secs(2),
        }
    }
}

struct Lane {
    semaphore: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_waiting: usize,
}

impl Lane {
    fn new(limits: LaneLimits) -> Self {
        Self {
            // A lane with zero permits would reject everything forever: that is a
            // configuration mistake, so clamp instead of building a dead gate.
            semaphore: Arc::new(Semaphore::new(limits.permits.max(1))),
            waiting: AtomicUsize::new(0),
            max_waiting: limits.max_waiting,
        }
    }
}

/// Decrements the waiter count on every exit path, including cancellation.
struct WaitingGuard<'a>(&'a AtomicUsize);

impl Drop for WaitingGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Bounded admission for blocking jobs. Cheap to share behind an `Arc`.
pub struct BlockingGate {
    password: Lane,
    general: Lane,
    permit_wait: Duration,
}

impl std::fmt::Debug for BlockingGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingGate")
            .field("password_available", &self.available(GateClass::Password))
            .field("general_available", &self.available(GateClass::General))
            .finish_non_exhaustive()
    }
}

impl BlockingGate {
    /// Builds a gate from its configuration.
    #[must_use]
    pub fn new(config: GateConfig) -> Self {
        Self {
            password: Lane::new(config.password),
            general: Lane::new(config.general),
            permit_wait: config.permit_wait,
        }
    }

    const fn lane(&self, class: GateClass) -> &Lane {
        match class {
            GateClass::Password => &self.password,
            GateClass::General => &self.general,
        }
    }

    /// Free permits in a lane (diagnostics and tests).
    #[must_use]
    pub fn available(&self, class: GateClass) -> usize {
        self.lane(class).semaphore.available_permits()
    }

    /// `acquire_before` without a request deadline, the spelling the tests use.
    #[cfg(test)]
    pub(crate) async fn acquire(
        &self,
        class: GateClass,
    ) -> Result<OwnedSemaphorePermit, AuthError> {
        self.acquire_before(class, None).await
    }

    /// Like `acquire`, but no wait outlives `deadline` (the request deadline of
    /// `AuthContext`). A deadline that has already passed refuses the call before it takes a
    /// permit, even a free one: the request is dead, and hashing for it would only occupy a
    /// core that a live request needs.
    pub(crate) async fn acquire_before(
        &self,
        class: GateClass,
        deadline: Option<Instant>,
    ) -> Result<OwnedSemaphorePermit, AuthError> {
        let wait = match deadline {
            None => self.permit_wait,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(Self::reject(class, "request deadline passed"));
                }
                // The shorter of the two bounds wins: `permit_wait` keeps the wait finite
                // for a caller whose deadline is far away.
                left.min(self.permit_wait)
            }
        };
        let lane = self.lane(class);
        // Fast path: a free permit costs no queueing and no timer.
        if let Ok(permit) = lane.semaphore.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let queued = lane.waiting.fetch_add(1, Ordering::AcqRel);
        let _guard = WaitingGuard(&lane.waiting);
        if queued >= lane.max_waiting {
            return Err(Self::reject(class, "queue full"));
        }
        match tokio::time::timeout(wait, lane.semaphore.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_closed)) => Err(Self::reject(class, "gate closed")),
            Err(_elapsed) => Err(Self::reject(class, "no permit before the deadline")),
        }
    }

    fn reject(class: GateClass, reason: &'static str) -> AuthError {
        metrics::counter!("rustryx_auth_blocking_rejected_total", "lane" => class.label())
            .increment(1);
        tracing::debug!(lane = class.label(), reason, "blocking work rejected");
        AuthError::Overloaded
    }

    /// Runs `work` on the blocking pool once a permit is held.
    ///
    /// Returns `AuthError::Overloaded` (503 with `Retry-After`) when no permit
    /// became available in time or the waiting room is full, and
    /// `AuthError::Backend` when the job panicked.
    ///
    /// Cancel safe: dropping the future before the permit is granted leaves no
    /// state; dropping it afterwards lets the job finish and release its permit.
    pub async fn run<T, F>(&self, class: GateClass, work: F) -> Result<T, AuthError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.run_before(class, None, work).await
    }

    /// Like [`Self::run`], with the request deadline bounding the wait for a permit (the
    /// job itself cannot be interrupted once it runs, see invariant 2).
    pub async fn run_before<T, F>(
        &self,
        class: GateClass,
        deadline: Option<Instant>,
        work: F,
    ) -> Result<T, AuthError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self.acquire_before(class, deadline).await?;
        let handle = tokio::task::spawn_blocking(move || {
            let output = work();
            drop(permit);
            output
        });
        handle
            .await
            .map_err(|error| backend_failure("blocking task", error))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    // `resume_unwind` panics without invoking the hook, which keeps test output quiet,
    // and sidesteps the workspace `clippy::panic` deny.
    fn explode() {
        std::panic::resume_unwind(Box::new("boom"));
    }

    fn tiny(permits: usize, max_waiting: usize, wait_ms: u64) -> BlockingGate {
        BlockingGate::new(GateConfig {
            password: LaneLimits {
                permits,
                max_waiting,
            },
            general: LaneLimits {
                permits,
                max_waiting,
            },
            permit_wait: Duration::from_millis(wait_ms),
        })
    }

    #[tokio::test]
    async fn must_not_wait_past_the_request_deadline() {
        // The configured wait is five seconds; the request has 60 ms left. The caller must be
        // refused after about 60 ms, not after five seconds.
        let gate = tiny(1, 4, 5_000);
        let _held = gate.acquire(GateClass::Password).await.unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(60);
        let error = gate
            .acquire_before(GateClass::Password, Some(deadline))
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{error:?}");
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(50) && waited < Duration::from_secs(2),
            "{waited:?}"
        );
    }

    #[tokio::test]
    async fn must_not_take_a_free_permit_for_a_request_whose_deadline_has_passed() {
        let gate = tiny(2, 2, 100);
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let error = gate
            .run_before(GateClass::Password, Some(Instant::now()), move || {
                flag.store(true, Ordering::SeqCst);
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{error:?}");
        assert!(!ran.load(Ordering::SeqCst), "the job must not start");
        assert_eq!(gate.available(GateClass::Password), 2);
    }

    #[tokio::test]
    async fn a_far_deadline_does_not_lengthen_the_configured_wait() {
        let gate = tiny(1, 4, 40);
        let _held = gate.acquire(GateClass::Password).await.unwrap();
        let started = Instant::now();
        let far = started + Duration::from_secs(600);
        let error = gate
            .acquire_before(GateClass::Password, Some(far))
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_deadline_that_is_still_open_lets_work_through() {
        let gate = tiny(1, 1, 100);
        let value = gate
            .run_before(
                GateClass::General,
                Some(Instant::now() + Duration::from_secs(30)),
                || 7,
            )
            .await
            .unwrap();
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn runs_work_and_returns_its_value() {
        let gate = tiny(2, 2, 100);
        let value = gate.run(GateClass::General, || 21 * 2).await.unwrap();
        assert_eq!(value, 42);
        assert_eq!(gate.available(GateClass::General), 2);
    }

    #[tokio::test]
    async fn must_not_grow_the_queue_past_the_waiting_limit() {
        let gate = Arc::new(tiny(1, 1, 5_000));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let running = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(GateClass::Password, move || {
                    let _ = release_rx.recv();
                })
                .await
            })
        };
        while gate.available(GateClass::Password) != 0 {
            tokio::task::yield_now().await;
        }
        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { gate.run(GateClass::Password, || ()).await })
        };
        while gate.password.waiting.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
        let started = Instant::now();
        let third = gate.run(GateClass::Password, || ()).await;
        assert!(matches!(third, Err(AuthError::Overloaded)));
        assert!(
            started.elapsed() < Duration::from_millis(1_000),
            "a full queue must reject immediately, not wait for the deadline"
        );
        release_tx.send(()).unwrap();
        running.await.unwrap().unwrap();
        waiter.await.unwrap().unwrap();
        assert_eq!(gate.password.waiting.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn must_not_wait_past_the_deadline_for_a_permit() {
        let gate = Arc::new(tiny(1, 4, 50));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let running = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(GateClass::Password, move || {
                    let _ = release_rx.recv();
                })
                .await
            })
        };
        while gate.available(GateClass::Password) != 0 {
            tokio::task::yield_now().await;
        }
        let result = gate.run(GateClass::Password, || ()).await;
        assert!(matches!(result, Err(AuthError::Overloaded)));
        assert_eq!(gate.password.waiting.load(Ordering::Acquire), 0);
        release_tx.send(()).unwrap();
        running.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn permit_is_taken_before_the_job_is_spawned() {
        let gate = Arc::new(tiny(1, 0, 10));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(GateClass::General, move || {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                })
                .await
            })
        };
        started_rx.await.unwrap();
        // The second job must never start: it would flip the flag if it did.
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let second = gate
            .run(GateClass::General, move || {
                flag.store(true, Ordering::SeqCst);
            })
            .await;
        assert!(matches!(second, Err(AuthError::Overloaded)));
        assert!(!ran.load(Ordering::SeqCst), "job ran without a permit");
        release_tx.send(()).unwrap();
        first.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_the_lane_busy_until_the_job_ends() {
        let gate = Arc::new(tiny(1, 0, 10));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let caller = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(GateClass::Password, move || {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                })
                .await
            })
        };
        started_rx.await.unwrap();
        caller.abort();
        let _ = caller.await;
        assert_eq!(
            gate.available(GateClass::Password),
            0,
            "the blocking job still runs, so its permit must still be held"
        );
        release_tx.send(()).unwrap();
        while gate.available(GateClass::Password) != 1 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn panicking_job_is_a_backend_error_and_frees_the_permit() {
        let gate = tiny(1, 1, 100);
        let result = gate.run(GateClass::General, explode).await;
        assert!(matches!(
            result,
            Err(AuthError::Backend {
                context: "blocking task",
                ..
            })
        ));
        assert_eq!(gate.available(GateClass::General), 1);
    }

    #[test]
    fn default_config_is_sized_from_the_machine() {
        let config = GateConfig::default();
        assert!(config.password.permits >= 1);
        assert!(config.general.permits >= config.password.permits);
        assert_eq!(config.general.permits % 2, 0);
    }

    #[test]
    fn zero_permit_lane_is_clamped_to_one() {
        let gate = tiny(0, 0, 10);
        assert_eq!(gate.available(GateClass::Password), 1);
        assert!(format!("{gate:?}").contains("password_available"));
    }
}
