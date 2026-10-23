//! Durable job queues with leases (`job_leases`).
//!
//! Workers on any node call [`PgJobLeases::claim`]. The claim is a single statement using
//! `FOR UPDATE SKIP LOCKED`: concurrent claimers skip rows another claimer holds instead of
//! waiting, so no two workers get the same row and nobody blocks. A worker that dies stops
//! calling [`PgJobLeases::heartbeat`], its lease expires and the next claim takes the row
//! over (SRE-23). Time comes from the database (`now()`), not from the node, so clock skew
//! between nodes cannot let two of them believe they hold the same lease.

use std::time::Duration;

use rustryx_core::StoreError;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::map_err;

/// A claimed job.
#[derive(Debug, Clone, FromRow)]
pub struct JobLease {
    /// Row id.
    pub id: Uuid,
    /// Queue name.
    pub queue: String,
    /// Job parameters.
    pub payload: serde_json::Value,
    /// How many times the row has been claimed, this claim included.
    pub attempts: i32,
    /// When the lease expires unless it is renewed.
    pub lease_expires_at: OffsetDateTime,
}

/// Queue operations over `job_leases`.
#[derive(Debug, Clone)]
pub struct PgJobLeases {
    pool: PgPool,
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

impl PgJobLeases {
    /// Creates the accessor over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Queues a job. With a `dedupe_key`, a job with the same `(queue, key)` that is still
    /// queued or running makes this a no-op; returns `true` when a row was inserted.
    pub async fn enqueue(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "INSERT INTO job_leases (id, queue, dedupe_key, payload) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (queue, dedupe_key) WHERE dedupe_key IS NOT NULL \
                 AND state IN ('queued', 'running') DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(queue)
        .bind(dedupe_key)
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("enqueue job", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Claims the oldest available job of `queue` for `owner` for `lease`, or `None`. A
    /// running job whose lease expired counts as available.
    pub async fn claim(
        &self,
        queue: &str,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<JobLease>, StoreError> {
        sqlx::query_as::<_, JobLease>(
            "UPDATE job_leases SET state = 'running', lease_owner = $2, attempts = attempts + 1, \
                 lease_expires_at = now() + make_interval(secs => $3), heartbeat_at = now(), \
                 updated_at = now() \
             WHERE id = ( \
                 SELECT id FROM job_leases \
                 WHERE queue = $1 AND available_at <= now() \
                   AND (state = 'queued' OR (state = 'running' AND lease_expires_at < now())) \
                 ORDER BY available_at, id FOR UPDATE SKIP LOCKED LIMIT 1) \
             RETURNING id, queue, payload, attempts, lease_expires_at",
        )
        .bind(queue)
        .bind(owner)
        .bind(seconds(lease))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("claim job", e))
    }

    /// Extends the lease. Returns `false` when `owner` no longer holds the job (it expired
    /// and was taken over): the worker must stop and write nothing more.
    pub async fn heartbeat(
        &self,
        id: Uuid,
        owner: &str,
        lease: Duration,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE job_leases SET lease_expires_at = now() + make_interval(secs => $3), \
                 heartbeat_at = now(), updated_at = now() \
             WHERE id = $1 AND lease_owner = $2 AND state = 'running' AND lease_expires_at >= now()",
        )
        .bind(id)
        .bind(owner)
        .bind(seconds(lease))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("renew job lease", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Marks the job done. Returns `false` when `owner` no longer holds it.
    pub async fn complete(&self, id: Uuid, owner: &str) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE job_leases SET state = 'done', lease_expires_at = NULL, updated_at = now() \
             WHERE id = $1 AND lease_owner = $2 AND state = 'running'",
        )
        .bind(id)
        .bind(owner)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("complete job", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Puts the job back to the queue to run again after `retry_in`, remembering `error`
    /// (a short, non-secret description). Returns `false` when `owner` no longer holds it.
    pub async fn release_for_retry(
        &self,
        id: Uuid,
        owner: &str,
        retry_in: Duration,
        error: &str,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE job_leases SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL, \
                 available_at = now() + make_interval(secs => $3), last_error = left($4, 500), \
                 updated_at = now() \
             WHERE id = $1 AND lease_owner = $2 AND state = 'running'",
        )
        .bind(id)
        .bind(owner)
        .bind(seconds(retry_in))
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("release job", e))?;
        Ok(result.rows_affected() == 1)
    }
}
