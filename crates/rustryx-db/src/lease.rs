//! Durable job queues with leases (`job_leases`), the [`JobLeaseRepo`] port.
//!
//! Workers on any node call [`JobLeaseRepo::claim`]. The claim is a single statement using
//! `FOR UPDATE SKIP LOCKED`: concurrent claimers skip rows another claimer holds instead of
//! waiting, so no two workers get the same row and nobody blocks. A worker that dies stops
//! calling [`JobLeaseRepo::heartbeat`], its lease expires and the next claim takes the row
//! over (SRE-23). Time comes from the database (`now()`), not from the node, so clock skew
//! between nodes cannot let two of them believe they hold the same lease.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::events::CursorAdvance;
use rustryx_core::ports::{JobLease, JobLeaseRepo, NewJob};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::map_err;
use crate::outbox;

/// Longest queue name the table accepts (its CHECK), checked here so that an overlong name is
/// `Invalid` input and not a constraint `Conflict`.
const MAX_QUEUE_LEN: usize = 128;
/// Longest stored error text, in characters. `release_for_retry` and `fail` truncate
/// in SQL with `left(.., 500)`; a longer text is not an error, it is shortened.
const MAX_ERROR_CHARS: i32 = 500;

#[derive(Debug, FromRow)]
struct LeaseRow {
    id: Uuid,
    queue: String,
    payload: serde_json::Value,
    attempts: i32,
    lease_expires_at: OffsetDateTime,
}

impl From<LeaseRow> for JobLease {
    fn from(row: LeaseRow) -> Self {
        Self {
            id: row.id,
            queue: row.queue,
            payload: row.payload,
            attempts: row.attempts,
            lease_expires_at: row.lease_expires_at,
        }
    }
}

/// Queue operations over `job_leases`.
#[derive(Debug, Clone)]
pub struct PgJobLeases {
    pool: PgPool,
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn check_queue(queue: &str) -> Result<(), StoreError> {
    if queue.is_empty() || queue.len() > MAX_QUEUE_LEN {
        return Err(StoreError::Invalid(
            "queue name must be 1 to 128 bytes".to_owned(),
        ));
    }
    Ok(())
}

impl PgJobLeases {
    /// Creates the accessor over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The columns of one new row, so single and batch inserts share one statement shape.
struct NewRows {
    queues: Vec<String>,
    dedupe_keys: Vec<Option<String>>,
    payloads: Vec<serde_json::Value>,
    delays: Vec<f64>,
}

impl NewRows {
    fn from_jobs(jobs: &[NewJob]) -> Result<Self, StoreError> {
        for job in jobs {
            check_queue(&job.queue)?;
        }
        Ok(Self {
            queues: jobs.iter().map(|j| j.queue.clone()).collect(),
            dedupe_keys: jobs.iter().map(|j| j.dedupe_key.clone()).collect(),
            payloads: jobs.iter().map(|j| j.payload.clone()).collect(),
            delays: jobs.iter().map(|j| seconds(j.delay)).collect(),
        })
    }
}

/// Inserts the rows and returns how many were inserted. A duplicate `(queue, dedupe_key)` of a
/// queued or running row is skipped, also within one batch: `ON CONFLICT DO NOTHING` sees the
/// rows this very statement inserted before.
async fn insert_rows(conn: &mut sqlx::PgConnection, rows: &NewRows) -> Result<u64, StoreError> {
    let ids: Vec<Uuid> = rows.queues.iter().map(|_| Uuid::now_v7()).collect();
    let result = sqlx::query(
        "INSERT INTO job_leases (id, queue, dedupe_key, payload, available_at) \
         SELECT id, queue, dedupe_key, payload, now() + make_interval(secs => delay) \
         FROM unnest($1::uuid[], $2::text[], $3::text[], $4::jsonb[], $5::float8[]) \
              WITH ORDINALITY AS t(id, queue, dedupe_key, payload, delay, ord) \
         ORDER BY ord \
         ON CONFLICT (queue, dedupe_key) WHERE dedupe_key IS NOT NULL \
             AND state IN ('queued', 'running') DO NOTHING",
    )
    .bind(&ids)
    .bind(&rows.queues)
    .bind(&rows.dedupe_keys)
    .bind(&rows.payloads)
    .bind(&rows.delays)
    .execute(conn)
    .await
    .map_err(|e| map_err("enqueue jobs", e))?;
    Ok(result.rows_affected())
}

#[async_trait]
impl JobLeaseRepo for PgJobLeases {
    /// Queues a job. With a `dedupe_key`, a job with the same `(queue, key)` that is still
    /// queued or running makes this a no-op; returns `true` when a row was inserted.
    async fn enqueue(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        self.enqueue_after(queue, dedupe_key, payload, Duration::ZERO)
            .await
    }

    async fn enqueue_after(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
        delay: Duration,
    ) -> Result<bool, StoreError> {
        check_queue(queue)?;
        let result = sqlx::query(
            "INSERT INTO job_leases (id, queue, dedupe_key, payload, available_at) \
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5)) \
             ON CONFLICT (queue, dedupe_key) WHERE dedupe_key IS NOT NULL \
                 AND state IN ('queued', 'running') DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(queue)
        .bind(dedupe_key)
        .bind(payload)
        .bind(seconds(delay))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("enqueue job", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Claims the oldest available job of `queue` for `owner` for `lease`, or `None`. A
    /// running job whose lease expired counts as available.
    async fn claim(
        &self,
        queue: &str,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<JobLease>, StoreError> {
        let row = sqlx::query_as::<_, LeaseRow>(
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
        .map_err(|e| map_err("claim job", e))?;
        Ok(row.map(JobLease::from))
    }

    /// Extends the lease. Returns `false` when `owner` no longer holds the job (it expired
    /// and was taken over): the worker must stop and write nothing more.
    async fn heartbeat(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StoreError> {
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
    async fn complete(&self, id: Uuid, owner: &str) -> Result<bool, StoreError> {
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
    async fn release_for_retry(
        &self,
        id: Uuid,
        owner: &str,
        retry_in: Duration,
        error: &str,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE job_leases SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL, \
                 available_at = now() + make_interval(secs => $3), last_error = left($4, $5), \
                 updated_at = now() \
             WHERE id = $1 AND lease_owner = $2 AND state = 'running'",
        )
        .bind(id)
        .bind(owner)
        .bind(seconds(retry_in))
        .bind(error)
        .bind(MAX_ERROR_CHARS)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("release job", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Terminal failure: state `failed`, the lease is cleared, the error is kept (at most 500
    /// characters). A failed row no longer counts for the dedupe key, so the same logical job
    /// can be queued again.
    async fn fail(&self, id: Uuid, owner: &str, error: &str) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE job_leases SET state = 'failed', lease_expires_at = NULL, \
                 last_error = left($3, $4), updated_at = now() \
             WHERE id = $1 AND lease_owner = $2 AND state = 'running'",
        )
        .bind(id)
        .bind(owner)
        .bind(error)
        .bind(MAX_ERROR_CHARS)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("fail job", e))?;
        Ok(result.rows_affected() == 1)
    }

    /// One transaction: the cursor move first (compare and set), then the rows. A cursor that
    /// moved meanwhile makes the call a no-op that returns 0 and inserts nothing, which is how
    /// a consumer that lost its lock or a duplicate consumer drops a batch another instance
    /// already handled: the rows of that batch are then written exactly once, by the winner.
    async fn enqueue_batch(
        &self,
        jobs: &[NewJob],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let rows = NewRows::from_jobs(jobs)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("enqueue jobs", e))?;
        if let Some(advance) = advance
            && !outbox::advance_cursor(&mut tx, advance).await?
        {
            return Ok(0);
        }
        let inserted = if jobs.is_empty() {
            0
        } else {
            insert_rows(&mut tx, &rows).await?
        };
        tx.commit().await.map_err(|e| map_err("enqueue jobs", e))?;
        Ok(inserted)
    }

    async fn depth(&self, queue: &str) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM job_leases WHERE queue = $1 AND state IN ('queued', 'running')",
        )
        .bind(queue)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("count jobs", e))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn purge_finished(&self, older_than: OffsetDateTime) -> Result<u64, StoreError> {
        let result = sqlx::query(
            "DELETE FROM job_leases WHERE state IN ('done', 'failed') AND updated_at < $1",
        )
        .bind(older_than)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("purge finished jobs", e))?;
        Ok(result.rows_affected())
    }
}
