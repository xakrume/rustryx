//! Revoked token ids, durable job queues and routing rules.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::audit::AuditEvent;
use rustryx_core::error::StoreError;
use rustryx_core::events::CursorAdvance;
use rustryx_core::ids::{RepositoryId, RoutingRuleId};
use rustryx_core::ports::{
    AuditQuery, AuditReadRepo, AuditSink, JobLease, JobLeaseRepo, NewJob, Page, PageRequest,
    RevokedTokenRepo, StoredAuditEvent,
};
use rustryx_core::routing::{RoutingRule, RoutingRuleRepo};
use time::OffsetDateTime;
use uuid::Uuid;

use super::state::{JobRow, JobState, State};
use super::support::bump_version;
use super::{MemStore, conflict, not_found};
use crate::support::add_std;

/// Longest error text a job keeps, in characters (the column limit of the real table).
const MAX_JOB_ERROR_CHARS: usize = 500;

#[async_trait]
impl RevokedTokenRepo for MemStore {
    async fn revoke(&self, tid: Uuid, expires_at: OffsetDateTime) -> Result<(), StoreError> {
        // Idempotent like `INSERT ... ON CONFLICT DO NOTHING`: the first expiry stays.
        self.tables()?
            .revoked_tokens
            .entry(tid)
            .or_insert(expires_at);
        Ok(())
    }

    async fn is_revoked(&self, tid: Uuid) -> Result<bool, StoreError> {
        // A row past its expiry still answers `true` until the purge removes it: the JWT is
        // expired by then, and the validator rejects it for that reason first.
        Ok(self.tables()?.revoked_tokens.contains_key(&tid))
    }

    async fn purge_expired(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let before = state.revoked_tokens.len();
        state
            .revoked_tokens
            .retain(|_, expires_at| *expires_at >= now);
        Ok(u64::try_from(before - state.revoked_tokens.len()).unwrap_or(u64::MAX))
    }
}

fn truncate_error(error: &str) -> String {
    error.chars().take(MAX_JOB_ERROR_CHARS).collect()
}

impl State {
    /// A queued or running row with the same `(queue, dedupe_key)` exists.
    fn job_is_duplicate(&self, queue: &str, dedupe_key: Option<&str>) -> bool {
        let Some(key) = dedupe_key else {
            return false;
        };
        self.jobs.iter().any(|job| {
            job.queue == queue
                && job.dedupe_key.as_deref() == Some(key)
                && matches!(job.state, JobState::Queued | JobState::Running)
        })
    }

    pub(crate) fn insert_job(
        &mut self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
        available_at: OffsetDateTime,
    ) -> bool {
        if self.job_is_duplicate(queue, dedupe_key) {
            return false;
        }
        self.jobs.push(JobRow {
            id: Uuid::now_v7(),
            queue: queue.to_owned(),
            dedupe_key: dedupe_key.map(str::to_owned),
            payload: payload.clone(),
            state: JobState::Queued,
            attempts: 0,
            owner: None,
            available_at,
            lease_expires_at: None,
            error: None,
            finished_at: None,
        });
        true
    }

    /// The running row `id` that `owner` still holds, or `None` when the lease was lost
    /// (the row finished, went back to the queue or was claimed by someone else).
    fn held_job(&mut self, id: Uuid, owner: &str) -> Option<&mut JobRow> {
        self.jobs.iter_mut().find(|job| {
            job.id == id && job.state == JobState::Running && job.owner.as_deref() == Some(owner)
        })
    }
}

#[async_trait]
impl JobLeaseRepo for MemStore {
    async fn enqueue(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        Ok(self.tables()?.insert_job(queue, dedupe_key, payload, now))
    }

    async fn enqueue_after(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
        delay: Duration,
    ) -> Result<bool, StoreError> {
        let available_at = add_std(self.now(), delay);
        Ok(self
            .tables()?
            .insert_job(queue, dedupe_key, payload, available_at))
    }

    async fn claim(
        &self,
        queue: &str,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<JobLease>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // Oldest first. A running row whose lease ran out is claimable again: its worker is
        // presumed dead, which is what a lease is for.
        let claimable = |job: &&mut JobRow| {
            job.queue == queue
                && match job.state {
                    JobState::Queued => job.available_at <= now,
                    JobState::Running => job.lease_expires_at.is_some_and(|at| at <= now),
                    JobState::Done | JobState::Failed => false,
                }
        };
        let Some(job) = state.jobs.iter_mut().find(claimable) else {
            return Ok(None);
        };
        let lease_expires_at = add_std(now, lease);
        job.state = JobState::Running;
        job.owner = Some(owner.to_owned());
        job.attempts = job.attempts.saturating_add(1);
        job.lease_expires_at = Some(lease_expires_at);
        Ok(Some(JobLease {
            id: job.id,
            queue: job.queue.clone(),
            payload: job.payload.clone(),
            attempts: job.attempts,
            lease_expires_at,
        }))
    }

    async fn heartbeat(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // A lease that ran out but was not claimed by anyone can still be renewed, the same
        // rule `LockGuard::renew` has: the lease is lost when someone took it over.
        let Some(job) = state.held_job(id, owner) else {
            return Ok(false);
        };
        job.lease_expires_at = Some(add_std(now, lease));
        Ok(true)
    }

    async fn complete(&self, id: Uuid, owner: &str) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(job) = state.held_job(id, owner) else {
            return Ok(false);
        };
        job.state = JobState::Done;
        job.finished_at = Some(now);
        job.lease_expires_at = None;
        Ok(true)
    }

    async fn release_for_retry(
        &self,
        id: Uuid,
        owner: &str,
        retry_in: Duration,
        error: &str,
    ) -> Result<bool, StoreError> {
        let available_at = add_std(self.now(), retry_in);
        let mut state = self.tables()?;
        let Some(job) = state.held_job(id, owner) else {
            return Ok(false);
        };
        job.state = JobState::Queued;
        job.owner = None;
        job.lease_expires_at = None;
        job.available_at = available_at;
        job.error = Some(truncate_error(error));
        Ok(true)
    }

    async fn fail(&self, id: Uuid, owner: &str, error: &str) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(job) = state.held_job(id, owner) else {
            return Ok(false);
        };
        job.state = JobState::Failed;
        job.error = Some(truncate_error(error));
        job.finished_at = Some(now);
        job.lease_expires_at = None;
        Ok(true)
    }

    async fn enqueue_batch(
        &self,
        jobs: &[NewJob],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // The cursor compare and set comes first and decides the whole call: a cursor that
        // moved meanwhile means another instance already handed this batch off.
        if let Some(advance) = advance {
            let current = state
                .outbox_cursors
                .get(advance.consumer)
                .copied()
                .unwrap_or_default();
            if current != advance.from {
                return Ok(0);
            }
            state
                .outbox_cursors
                .insert(advance.consumer.to_owned(), advance.to);
        }
        let mut inserted = 0_u64;
        for job in jobs {
            let available_at = add_std(now, job.delay);
            if state.insert_job(
                &job.queue,
                job.dedupe_key.as_deref(),
                &job.payload,
                available_at,
            ) {
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    async fn depth(&self, queue: &str) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let depth = state
            .jobs
            .iter()
            .filter(|job| job.queue == queue)
            .filter(|job| matches!(job.state, JobState::Queued | JobState::Running))
            .count();
        Ok(u64::try_from(depth).unwrap_or(u64::MAX))
    }

    async fn purge_finished(&self, older_than: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let before = state.jobs.len();
        state.jobs.retain(|job| {
            let finished = matches!(job.state, JobState::Done | JobState::Failed);
            !(finished && job.finished_at.is_some_and(|at| at < older_than))
        });
        Ok(u64::try_from(before - state.jobs.len()).unwrap_or(u64::MAX))
    }
}

#[async_trait]
impl RoutingRuleRepo for MemStore {
    async fn create(&self, rule: &RoutingRule) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state
            .routing_rules
            .values()
            .any(|r| r.id == rule.id || r.name == rule.name);
        if taken {
            return Err(conflict(format!(
                "routing rule {} already exists",
                rule.name
            )));
        }
        state.routing_rules.insert(rule.id, rule.clone());
        Ok(())
    }

    async fn get(&self, id: RoutingRuleId) -> Result<Option<RoutingRule>, StoreError> {
        Ok(self.tables()?.routing_rules.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<RoutingRule>, StoreError> {
        let mut all: Vec<RoutingRule> = self.tables()?.routing_rules.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(&self, rule: &RoutingRule, expected_version: i64) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .routing_rules
            .get(&rule.id)
            .ok_or_else(|| not_found("routing rule", rule.id))?;
        let version = bump_version(existing.version, expected_version)?;
        let created_at = existing.created_at;
        let name_taken = state
            .routing_rules
            .values()
            .any(|r| r.id != rule.id && r.name == rule.name);
        if name_taken {
            return Err(conflict(format!(
                "routing rule {} already exists",
                rule.name
            )));
        }
        let mut next = rule.clone();
        next.version = version;
        next.created_at = created_at;
        next.updated_at = now;
        state.routing_rules.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: RoutingRuleId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.routing_rules.contains_key(&id) {
            return Err(not_found("routing rule", id));
        }
        if state
            .repositories
            .values()
            .any(|r| r.routing_rule_id == Some(id))
        {
            return Err(conflict("routing rule is still used by a repository"));
        }
        state.routing_rules.remove(&id);
        Ok(())
    }

    async fn assigned_to(&self, id: RoutingRuleId) -> Result<Vec<RepositoryId>, StoreError> {
        let state = self.tables()?;
        let mut repositories: Vec<RepositoryId> = state
            .repositories
            .values()
            .filter(|r| r.routing_rule_id == Some(id))
            .map(|r| r.id)
            .collect();
        repositories.sort();
        Ok(repositories)
    }
}

#[async_trait]
impl AuditSink for MemStore {
    async fn record(&self, event: AuditEvent) -> Result<(), StoreError> {
        // The availability switch covers the audit table like every other table.
        self.check_available()?;
        self.audit.record(event).await
    }
}

#[async_trait]
impl AuditReadRepo for MemStore {
    async fn query(
        &self,
        query: &AuditQuery,
        page: &PageRequest,
    ) -> Result<Page<StoredAuditEvent>, StoreError> {
        self.check_available()?;
        self.audit.query_page(query, page)
    }

    fn export(
        &self,
        query: &AuditQuery,
    ) -> BoxStream<'static, Result<StoredAuditEvent, StoreError>> {
        // Fail inside the stream, not at the call: the port is synchronous and the consumer
        // handles errors where it reads them.
        if self.check_available().is_err() {
            let error = StoreError::Unavailable("memory store marked unavailable".to_owned());
            return futures::stream::once(async move { Err(error) }).boxed();
        }
        futures::stream::iter(self.audit.export_rows(query).into_iter().map(Ok)).boxed()
    }
}
