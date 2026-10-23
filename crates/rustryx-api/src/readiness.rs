//! Readiness: the probe port, its report, and the cache in front of it.
//!
//! `/readyz` answers 503 only when this node cannot serve any metadata request:
//! it is not listening yet, it is draining, the schema is older than the binary
//! expects, or PostgreSQL did not answer a `SELECT 1` within one second. Redis,
//! Keycloak, LDAP, S3 and upstream registries never change the status code
//! (they may appear as advisories in the body). That is deliberate:
//!
//! - **Why PostgreSQL is in.** Without the database a node cannot serve a single
//!   metadata request, so pulling it out of the load balancer is correct. The
//!   price is a cascade: when PostgreSQL is down every node turns `NotReady` at
//!   once. That risk is accepted knowingly; `/healthz` does not depend on the
//!   database, so Kubernetes does not restart the pods and traffic returns by
//!   itself when the database does.
//! - **Why nothing else is in.** Each other dependency has a fallback or a
//!   narrower blast radius (local accounts and `ryx_` tokens work without
//!   Keycloak, cached artifacts without S3 or upstream, locks without Redis).
//!   Making them readiness inputs would take the whole service out for a partial
//!   failure and restart-loop-adjacent behaviour for nothing.

use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

/// Result of one dependency check. Deliberately carries no detail: the body of
/// `/readyz` is unauthenticated and must not contain DSNs, hosts or driver
/// messages. The implementation logs the detail itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The check passed.
    Ok,
    /// The check failed or timed out.
    Failed,
}

/// How the database schema compares with what this binary expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaStatus {
    /// Exactly the expected version.
    UpToDate,
    /// Newer than this binary knows (a rollback of the application): served in
    /// compatibility mode, which the expand/contract rule makes safe.
    Ahead,
    /// Older than expected: `rustryx migrate` has not run yet.
    Behind,
    /// The version could not be read.
    Unknown,
}

impl SchemaStatus {
    /// True when the node may serve with this schema.
    #[must_use]
    pub fn is_servable(self) -> bool {
        matches!(self, Self::UpToDate | Self::Ahead)
    }
}

/// A dependency that is reported but never decides the status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// Short name such as `redis`.
    pub name: &'static str,
    /// Whether the dependency is healthy.
    pub healthy: bool,
}

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// Did PostgreSQL answer `SELECT 1`?
    pub database: Outcome,
    /// Schema version check.
    pub schema: SchemaStatus,
    /// Informational dependencies (blob store, Redis, ...).
    pub advisories: Vec<Advisory>,
}

impl ProbeReport {
    /// A report for a node whose dependencies are all fine.
    #[must_use]
    pub fn healthy() -> Self {
        Self {
            database: Outcome::Ok,
            schema: SchemaStatus::UpToDate,
            advisories: Vec::new(),
        }
    }
}

/// Port implemented by the composition root (it owns the database pool).
#[async_trait]
pub trait ReadinessProbe: Send + Sync {
    /// Runs `SELECT 1` and reads the schema version. Cancel safe: the caller
    /// abandons the future after one second. Must not log or return secrets.
    async fn check(&self) -> ProbeReport;
}

/// Probe used until a database is wired: the node reports not ready.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotWiredProbe;

#[async_trait]
impl ReadinessProbe for NotWiredProbe {
    async fn check(&self) -> ProbeReport {
        ProbeReport {
            database: Outcome::Failed,
            schema: SchemaStatus::Unknown,
            advisories: Vec::new(),
        }
    }
}

/// Runs the probe with a deadline and remembers the answer briefly.
///
/// The cache exists because kubelet and load balancers poll `/readyz` every
/// second or so from several sources, and the endpoint is unauthenticated: without
/// it each poll (or an attacker's flood) would run a query on the connection pool.
#[derive(Debug)]
pub struct ReadinessCache {
    ttl: Duration,
    deadline: Duration,
    slot: Mutex<Option<(Instant, ProbeReport)>>,
}

impl ReadinessCache {
    /// Cache with the given time to live and probe deadline.
    #[must_use]
    pub fn new(ttl: Duration, deadline: Duration) -> Self {
        Self {
            ttl,
            deadline,
            slot: Mutex::new(None),
        }
    }

    /// The cached report if still fresh, otherwise a new probe run bounded by the deadline.
    pub async fn report(&self, probe: &dyn ReadinessProbe) -> ProbeReport {
        if let Some(fresh) = self.fresh() {
            return fresh;
        }
        let report = tokio::time::timeout(self.deadline, probe.check())
            .await
            .unwrap_or_else(|_| ProbeReport {
                database: Outcome::Failed,
                schema: SchemaStatus::Unknown,
                advisories: Vec::new(),
            });
        // The guard is dropped at the end of the statement, before any await.
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), report.clone()));
        report
    }

    fn fresh(&self) -> Option<ProbeReport> {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref()
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, report)| report.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Counting(AtomicUsize, Duration);

    #[async_trait]
    impl ReadinessProbe for Counting {
        async fn check(&self) -> ProbeReport {
            self.0.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.1).await;
            ProbeReport::healthy()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn answers_within_the_ttl_come_from_the_cache() {
        let probe = Counting(AtomicUsize::new(0), Duration::ZERO);
        let cache = ReadinessCache::new(Duration::from_secs(2), Duration::from_secs(1));
        for _ in 0..5 {
            assert_eq!(cache.report(&probe).await, ProbeReport::healthy());
        }
        assert_eq!(probe.0.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(3)).await;
        let _ = cache.report(&probe).await;
        assert_eq!(probe.0.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_probe_is_a_failure_after_the_deadline() {
        let probe = Counting(AtomicUsize::new(0), Duration::from_secs(5));
        let cache = ReadinessCache::new(Duration::from_secs(2), Duration::from_secs(1));
        let started = Instant::now();
        let report = cache.report(&probe).await;
        assert_eq!(report.database, Outcome::Failed);
        assert_eq!(report.schema, SchemaStatus::Unknown);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn only_current_or_newer_schemas_are_servable() {
        assert!(SchemaStatus::UpToDate.is_servable());
        assert!(SchemaStatus::Ahead.is_servable());
        assert!(!SchemaStatus::Behind.is_servable());
        assert!(!SchemaStatus::Unknown.is_servable());
    }

    #[tokio::test]
    async fn unwired_probe_reports_not_ready_material() {
        let report = NotWiredProbe.check().await;
        assert_eq!(report.database, Outcome::Failed);
    }
}
