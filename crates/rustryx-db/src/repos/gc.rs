//! Garbage collection run history.

use rustryx_core::ids::{BlobStoreId, GcRunId, TaskRunId, UserId};
use rustryx_core::lifecycle::{GcCounts, GcOptions, GcRun, GcSample};
use rustryx_core::ports::{GcRunRepo, Page, PageRequest};
use rustryx_core::tasks::{TaskRunStatus, TaskTrigger};
use rustryx_core::StoreError;
use sqlx::PgPool;
use sqlx::types::Json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cursor;
use crate::error::map_err;
use crate::repos::common::{corrupt, to_i64, to_u64};

const PAGE: &str = "gc_run";
const SAMPLE_PAGE: &str = "gc_sample";
const MAX_SAMPLES: i32 = 1000;

const LIST_SQL: &str = "SELECT id, trigger, status, dry_run, delete_untagged, min_age_secs, \
     workers, blob_store_id, examined, candidates, deleted, failed, bytes_candidates, \
     bytes_deleted, untagged_deleted, per_store, stop_requested, task_run_id, requested_by, \
     started_at, finished_at, created_at FROM gc_runs \
     WHERE ($1::bool OR created_at < $2 OR (created_at = $2 AND id < $3)) \
     ORDER BY created_at DESC, id DESC LIMIT $4";

/// PostgreSQL [`GcRunRepo`].
#[derive(Debug, Clone)]
pub struct PgGcRunRepo {
    pool: PgPool,
}

impl PgGcRunRepo {
    /// Repo over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl GcRunRepo for PgGcRunRepo {
    async fn create(&self, run: &GcRun) -> Result<(), StoreError> {
        let trigger = trigger_text(run.trigger)?;
        let status = status_text(run.status)?;
        sqlx::query(
            "INSERT INTO gc_runs (id, trigger, status, dry_run, delete_untagged, min_age_secs, \
             workers, blob_store_id, examined, candidates, deleted, failed, bytes_candidates, \
             bytes_deleted, untagged_deleted, per_store, stop_requested, task_run_id, \
             requested_by, started_at, finished_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)",
        )
        .bind(run.id.as_uuid())
        .bind(trigger)
        .bind(status)
        .bind(run.options.dry_run)
        .bind(run.options.delete_untagged)
        .bind(to_i64(run.options.min_age_secs)?)
        .bind(i16::from(run.options.workers))
        .bind(run.options.blob_store.map(|id| id.as_uuid()))
        .bind(to_i64(run.counts.examined)?)
        .bind(to_i64(run.counts.candidates)?)
        .bind(to_i64(run.counts.deleted)?)
        .bind(to_i64(run.counts.failed)?)
        .bind(to_i64(run.counts.bytes_candidates)?)
        .bind(to_i64(run.counts.bytes_deleted)?)
        .bind(to_i64(run.counts.untagged_deleted)?)
        .bind(Json(&run.per_store))
        .bind(run.stop_requested)
        .bind(run.task_run_id.map(|id| id.as_uuid()))
        .bind(run.requested_by.map(|id| id.as_uuid()))
        .bind(run.started_at)
        .bind(run.finished_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create gc run", e))?;
        Ok(())
    }

    async fn get(&self, id: GcRunId) -> Result<Option<GcRun>, StoreError> {
        sqlx::query_as::<_, RunRow>(
            "SELECT id, trigger, status, dry_run, delete_untagged, min_age_secs, workers, \
             blob_store_id, examined, candidates, deleted, failed, bytes_candidates, \
             bytes_deleted, untagged_deleted, per_store, stop_requested, task_run_id, \
             requested_by, started_at, finished_at, created_at FROM gc_runs WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read gc run", e))?
        .map(RunRow::into_run)
        .transpose()
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<GcRun>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let (first, at, id) = match &page.after {
            None => (true, OffsetDateTime::UNIX_EPOCH, Uuid::nil()),
            Some(token) => run_cursor(token)?,
        };
        let rows: Vec<RunRow> = sqlx::query_as(LIST_SQL)
            .bind(first)
            .bind(at)
            .bind(id)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list gc runs", e))?;
        let mut stamped: Vec<(OffsetDateTime, GcRun)> = rows
            .into_iter()
            .map(|row| {
                let at = row.created_at;
                row.into_run().map(|run| (at, run))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next = if stamped.len() > limit {
            stamped.truncate(limit);
            stamped.last().map(|(at, run)| {
                cursor::encode(PAGE, &format!("{}|{}", at.unix_timestamp_nanos(), run.id))
            })
        } else {
            None
        };
        Ok(Page {
            items: stamped.into_iter().map(|(_, run)| run).collect(),
            next,
        })
    }

    async fn update_progress(
        &self,
        id: GcRunId,
        counts: &GcCounts,
        per_store: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE gc_runs SET examined = $2, candidates = $3, deleted = $4, failed = $5, \
             bytes_candidates = $6, bytes_deleted = $7, untagged_deleted = $8, per_store = $9 \
             WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(to_i64(counts.examined)?)
        .bind(to_i64(counts.candidates)?)
        .bind(to_i64(counts.deleted)?)
        .bind(to_i64(counts.failed)?)
        .bind(to_i64(counts.bytes_candidates)?)
        .bind(to_i64(counts.bytes_deleted)?)
        .bind(to_i64(counts.untagged_deleted)?)
        .bind(Json(per_store))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update gc progress", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("gc run".to_owned()));
        }
        Ok(())
    }

    async fn finish(
        &self,
        id: GcRunId,
        status: TaskRunStatus,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query("UPDATE gc_runs SET status = $2, finished_at = $3 WHERE id = $1")
            .bind(id.as_uuid())
            .bind(status_text(status)?)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("finish gc run", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("gc run".to_owned()));
        }
        Ok(())
    }

    async fn request_stop(&self, id: GcRunId) -> Result<bool, StoreError> {
        let updated = sqlx::query(
            "UPDATE gc_runs SET stop_requested = true \
             WHERE id = $1 AND status IN ('queued', 'running')",
        )
        .bind(id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("stop gc run", e))?;
        if updated.rows_affected() == 1 {
            return Ok(true);
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gc_runs WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("stop gc run", e))?;
        if exists {
            Ok(false)
        } else {
            Err(StoreError::NotFound("gc run".to_owned()))
        }
    }

    async fn stop_requested(&self, id: GcRunId) -> Result<bool, StoreError> {
        let flag: Option<bool> =
            sqlx::query_scalar("SELECT stop_requested FROM gc_runs WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read gc stop", e))?;
        flag.ok_or_else(|| StoreError::NotFound("gc run".to_owned()))
    }

    async fn save_samples(&self, samples: &[GcSample]) -> Result<(), StoreError> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut by_run: Vec<(GcRunId, Vec<&GcSample>)> = Vec::new();
        for sample in samples {
            if let Some((_, group)) = by_run.iter_mut().find(|(id, _)| *id == sample.run_id) {
                group.push(sample);
            } else {
                by_run.push((sample.run_id, vec![sample]));
            }
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("write gc samples", e))?;
        for (run_id, group) in by_run {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gc_runs WHERE id = $1)")
                    .bind(run_id.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("write gc samples", e))?;
            if !exists {
                return Err(StoreError::Conflict(format!(
                    "gc run {run_id} does not exist"
                )));
            }
            let mut stores = Vec::with_capacity(group.len());
            let mut keys = Vec::with_capacity(group.len());
            let mut sizes = Vec::with_capacity(group.len());
            let mut orphaned = Vec::with_capacity(group.len());
            for sample in group {
                stores.push(sample.blob_store.clone());
                keys.push(sample.key.clone());
                sizes.push(to_i64(sample.size_bytes)?);
                orphaned.push(sample.orphaned_at);
            }
            sqlx::query(
                "WITH incoming AS ( \
                     SELECT blob_store, object_key, size_bytes, orphaned_at, n::int AS n \
                     FROM unnest($2::text[], $3::text[], $4::bigint[], $5::timestamptz[]) \
                          WITH ORDINALITY AS t(blob_store, object_key, size_bytes, orphaned_at, n) \
                 ), base AS ( \
                     SELECT COALESCE(MAX(seq), 0) AS max_seq, COUNT(*)::int AS have \
                     FROM gc_samples WHERE run_id = $1 \
                 ) \
                 INSERT INTO gc_samples (run_id, seq, blob_store, object_key, size_bytes, orphaned_at) \
                 SELECT $1, base.max_seq + incoming.n, incoming.blob_store, incoming.object_key, \
                        incoming.size_bytes, incoming.orphaned_at \
                 FROM incoming, base \
                 WHERE incoming.n <= GREATEST($6::int - base.have, 0)",
            )
            .bind(run_id.as_uuid())
            .bind(&stores)
            .bind(&keys)
            .bind(&sizes)
            .bind(&orphaned)
            .bind(MAX_SAMPLES)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("write gc samples", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| map_err("write gc samples", e))
    }

    async fn list_samples(
        &self,
        run: GcRunId,
        page: &PageRequest,
    ) -> Result<Page<GcSample>, StoreError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gc_runs WHERE id = $1)")
            .bind(run.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("list gc samples", e))?;
        if !exists {
            return Err(StoreError::NotFound("gc run".to_owned()));
        }
        let (limit, fetch) = cursor::fetch_limit(page);
        let (first, seq) = match &page.after {
            None => (true, 0_i32),
            Some(token) => {
                let key = cursor::decode(SAMPLE_PAGE, token)?;
                let seq: i32 = key.parse().map_err(|_| cursor::invalid_token())?;
                (false, seq)
            }
        };
        let rows: Vec<SampleRow> = sqlx::query_as(
            "SELECT seq, blob_store, object_key, size_bytes, orphaned_at FROM gc_samples \
             WHERE run_id = $1 AND ($2::bool OR seq > $3) ORDER BY seq ASC LIMIT $4",
        )
        .bind(run.as_uuid())
        .bind(first)
        .bind(seq)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list gc samples", e))?;
        let next_key = |row: &SampleRow| row.seq.to_string();
        let mut samples = Vec::with_capacity(rows.len());
        for row in rows {
            samples.push((next_key(&row), row.into_sample(run)?));
        }
        let page = cursor::finish(samples, limit, SAMPLE_PAGE, |(key, _)| key.clone());
        Ok(Page {
            items: page.items.into_iter().map(|(_, sample)| sample).collect(),
            next: page.next,
        })
    }

    async fn last_manual_start(&self) -> Result<Option<OffsetDateTime>, StoreError> {
        let at: Option<OffsetDateTime> = sqlx::query_scalar(
            "SELECT MAX(COALESCE(started_at, created_at)) FROM gc_runs WHERE trigger = 'manual'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("read last manual gc", e))?;
        Ok(at)
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let removed: i64 = sqlx::query_scalar(
            "WITH gone AS (DELETE FROM gc_runs WHERE finished_at < $1 RETURNING id) \
             SELECT count(*)::bigint FROM gone",
        )
        .bind(finished_before)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("purge gc runs", e))?;
        to_u64(removed, "purge gc runs")
    }
}

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    trigger: String,
    status: String,
    dry_run: bool,
    delete_untagged: bool,
    min_age_secs: i64,
    workers: i16,
    blob_store_id: Option<Uuid>,
    examined: i64,
    candidates: i64,
    deleted: i64,
    failed: i64,
    bytes_candidates: i64,
    bytes_deleted: i64,
    untagged_deleted: i64,
    per_store: Json<serde_json::Value>,
    stop_requested: bool,
    task_run_id: Option<Uuid>,
    requested_by: Option<Uuid>,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl RunRow {
    fn into_run(self) -> Result<GcRun, StoreError> {
        let workers = u8::try_from(self.workers).map_err(|_| corrupt("read gc run"))?;
        let mut run = GcRun::new(
            GcRunId::from_uuid(self.id),
            parse_trigger(&self.trigger)?,
            GcOptions {
                dry_run: self.dry_run,
                delete_untagged: self.delete_untagged,
                min_age_secs: to_u64(self.min_age_secs, "read gc run")?,
                workers,
                blob_store: self.blob_store_id.map(BlobStoreId::from_uuid),
            },
        );
        run.status = parse_status(&self.status)?;
        run.counts = GcCounts {
            examined: to_u64(self.examined, "read gc run")?,
            candidates: to_u64(self.candidates, "read gc run")?,
            deleted: to_u64(self.deleted, "read gc run")?,
            failed: to_u64(self.failed, "read gc run")?,
            bytes_candidates: to_u64(self.bytes_candidates, "read gc run")?,
            bytes_deleted: to_u64(self.bytes_deleted, "read gc run")?,
            untagged_deleted: to_u64(self.untagged_deleted, "read gc run")?,
        };
        run.per_store = self.per_store.0;
        run.stop_requested = self.stop_requested;
        run.task_run_id = self.task_run_id.map(TaskRunId::from_uuid);
        run.requested_by = self.requested_by.map(UserId::from_uuid);
        run.started_at = self.started_at;
        run.finished_at = self.finished_at;
        Ok(run)
    }
}

#[derive(sqlx::FromRow)]
struct SampleRow {
    seq: i32,
    blob_store: String,
    object_key: String,
    size_bytes: i64,
    orphaned_at: OffsetDateTime,
}

impl SampleRow {
    fn into_sample(self, run: GcRunId) -> Result<GcSample, StoreError> {
        Ok(GcSample {
            run_id: run,
            blob_store: self.blob_store,
            key: self.object_key,
            size_bytes: to_u64(self.size_bytes, "read gc sample")?,
            orphaned_at: self.orphaned_at,
        })
    }
}

fn trigger_text(trigger: TaskTrigger) -> Result<&'static str, StoreError> {
    Ok(match trigger {
        TaskTrigger::Schedule => "schedule",
        TaskTrigger::Manual => "manual",
        TaskTrigger::Event => "event",
        TaskTrigger::Startup => "startup",
        _ => return Err(StoreError::Invalid("unknown gc trigger".to_owned())),
    })
}

fn parse_trigger(raw: &str) -> Result<TaskTrigger, StoreError> {
    match raw {
        "schedule" => Ok(TaskTrigger::Schedule),
        "manual" => Ok(TaskTrigger::Manual),
        "event" => Ok(TaskTrigger::Event),
        "startup" => Ok(TaskTrigger::Startup),
        _ => Err(corrupt("read gc run")),
    }
}

fn status_text(status: TaskRunStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        TaskRunStatus::Queued => "queued",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Succeeded => "succeeded",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Skipped => "skipped",
        TaskRunStatus::Abandoned => "abandoned",
        _ => return Err(StoreError::Invalid("unknown task status".to_owned())),
    })
}

fn parse_status(raw: &str) -> Result<TaskRunStatus, StoreError> {
    match raw {
        "queued" => Ok(TaskRunStatus::Queued),
        "running" => Ok(TaskRunStatus::Running),
        "succeeded" => Ok(TaskRunStatus::Succeeded),
        "failed" => Ok(TaskRunStatus::Failed),
        "cancelled" => Ok(TaskRunStatus::Cancelled),
        "skipped" => Ok(TaskRunStatus::Skipped),
        "abandoned" => Ok(TaskRunStatus::Abandoned),
        _ => Err(corrupt("read gc run")),
    }
}

fn run_cursor(token: &str) -> Result<(bool, OffsetDateTime, Uuid), StoreError> {
    let key = cursor::decode(PAGE, token)?;
    let (nanos, id) = key.split_once('|').ok_or_else(cursor::invalid_token)?;
    let nanos: i128 = nanos.parse().map_err(|_| cursor::invalid_token())?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| cursor::invalid_token())?;
    let id = Uuid::parse_str(id).map_err(|_| cursor::invalid_token())?;
    Ok((false, at, id))
}
