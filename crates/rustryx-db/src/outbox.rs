//! Writing to the event outbox (`event_outbox`, `outbox_cursors`) inside the transaction of a
//! state change.
//!
//! A state changing port method that takes `WriteOptions::events` inserts the rows in its own
//! transaction, so an event exists if and only if the change committed (design 2.2 rule 1).
//! This module is that insert plus the compare and set of a consumer cursor, which
//! [`crate::lease::PgJobLeases::enqueue_batch`] needs for the atomic hand-off. Reading
//! batches, lag and purge belong to the `OutboxRepo` implementation, not to this foundation.

use rustryx_core::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::error::map_err;

/// Channel that wakes the outbox consumers. The payload is empty: a consumer polls anyway,
/// the notification only shortens the latency.
pub const OUTBOX_CHANNEL: &str = "rustryx_outbox";

/// SQLSTATE of a row that no partition accepts (`check_violation`). `event_outbox` has no other
/// check constraint, so within the one statement below it can only mean "no partition for this
/// month".
const SQLSTATE_NO_PARTITION: &str = "23514";

/// Column arrays of a batch of drafts, built once so a retry does not rebuild them.
struct EventColumns {
    ids: Vec<Uuid>,
    kinds: Vec<&'static str>,
    projects: Vec<Option<Uuid>>,
    repositories: Vec<Option<Uuid>>,
    actor_ids: Vec<Option<Uuid>>,
    actor_names: Vec<String>,
    actor_kinds: Vec<&'static str>,
    requests: Vec<Option<String>>,
    data: Vec<serde_json::Value>,
}

impl EventColumns {
    fn new(events: &[EventDraft]) -> Self {
        Self {
            ids: events.iter().map(|_| Uuid::now_v7()).collect(),
            kinds: events.iter().map(|e| e.kind.as_str()).collect(),
            projects: events
                .iter()
                .map(|e| e.project_id.map(|id| id.as_uuid()))
                .collect(),
            repositories: events
                .iter()
                .map(|e| e.repository_id.map(|id| id.as_uuid()))
                .collect(),
            actor_ids: events
                .iter()
                .map(|e| e.actor.id.map(|id| id.as_uuid()))
                .collect(),
            actor_names: events.iter().map(|e| e.actor.name.clone()).collect(),
            actor_kinds: events.iter().map(|e| e.actor.kind.as_str()).collect(),
            requests: events.iter().map(|e| e.request_id.clone()).collect(),
            data: events.iter().map(|e| e.data.clone()).collect(),
        }
    }
}

/// One statement for the whole batch. `WITH ORDINALITY ... ORDER BY` pins the order in which
/// `nextval` is evaluated, so the sequence numbers follow the order of the drafts.
async fn insert_columns(conn: &mut PgConnection, c: &EventColumns) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO event_outbox (id, kind, project_id, repository_id, actor_id, actor_name, \
         actor_kind, request_id, data) \
         SELECT id, kind, project_id, repository_id, actor_id, actor_name, actor_kind, \
                request_id, data \
         FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::uuid[], $5::uuid[], $6::text[], \
                     $7::text[], $8::text[], $9::jsonb[]) \
              WITH ORDINALITY AS t(id, kind, project_id, repository_id, actor_id, actor_name, \
                                   actor_kind, request_id, data, ord) \
         ORDER BY ord",
    )
    .bind(&c.ids)
    .bind(&c.kinds)
    .bind(&c.projects)
    .bind(&c.repositories)
    .bind(&c.actor_ids)
    .bind(&c.actor_names)
    .bind(&c.actor_kinds)
    .bind(&c.requests)
    .bind(&c.data)
    .execute(conn)
    .await
    .map(|_| ())
}

fn is_missing_partition(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == SQLSTATE_NO_PARTITION)
}

/// Inserts `events` on `conn`, which is the open transaction of the state change, and queues a
/// wake-up for the consumers (delivered after commit only).
///
/// A payload over 16 KiB is `StoreError::Invalid` and nothing is written. When the month of the
/// event has no partition (the maintenance job fell behind) the partition is created inside
/// the same transaction and the insert is repeated once: failing a publish because a
/// housekeeping job was late would be worse than the short DDL lock. The first attempt runs in
/// a savepoint, because a failed statement would otherwise abort the caller's transaction.
pub(crate) async fn insert_events(
    conn: &mut PgConnection,
    events: &[EventDraft],
) -> Result<(), StoreError> {
    if events.is_empty() {
        return Ok(());
    }
    for event in events {
        event.validate()?;
    }
    let columns = EventColumns::new(events);
    let mut savepoint = conn
        .begin()
        .await
        .map_err(|e| map_err("write outbox events", e))?;
    match insert_columns(&mut savepoint, &columns).await {
        Ok(()) => savepoint
            .commit()
            .await
            .map_err(|e| map_err("write outbox events", e))?,
        Err(err) if is_missing_partition(&err) => {
            savepoint
                .rollback()
                .await
                .map_err(|e| map_err("write outbox events", e))?;
            sqlx::query("SELECT ensure_outbox_partitions(now(), 0)")
                .execute(&mut *conn)
                .await
                .map_err(|e| map_err("create outbox partition", e))?;
            insert_columns(conn, &columns)
                .await
                .map_err(|e| map_err("write outbox events", e))?;
        }
        Err(err) => return Err(map_err("write outbox events", err)),
    }
    sqlx::query("SELECT pg_notify($1, '')")
        .bind(OUTBOX_CHANNEL)
        .execute(conn)
        .await
        .map_err(|e| map_err("notify outbox consumers", e))?;
    Ok(())
}

/// Compare and set of a consumer cursor inside the caller's transaction. `true` when the
/// cursor was at `advance.from` and now is at `advance.to`; `false` when another instance
/// moved it first, in which case the caller drops its batch and writes nothing.
///
/// A consumer that never ran has no row, which is the position `(0, 0)`: the first advance
/// inserts the row, and only when `from` is that position. `tx_id` travels as text because
/// `sqlx` has no `xid8` type; PostgreSQL parses the decimal form.
pub(crate) async fn advance_cursor(
    conn: &mut PgConnection,
    advance: &CursorAdvance,
) -> Result<bool, StoreError> {
    let from_tx = advance.from.tx_id.to_string();
    let to_tx = advance.to.tx_id.to_string();
    let updated = sqlx::query(
        "UPDATE outbox_cursors SET last_tx_id = $4::text::xid8, last_seq = $5, updated_at = now() \
         WHERE consumer = $1 AND last_tx_id = $2::text::xid8 AND last_seq = $3",
    )
    .bind(advance.consumer)
    .bind(&from_tx)
    .bind(advance.from.seq)
    .bind(&to_tx)
    .bind(advance.to.seq)
    .execute(&mut *conn)
    .await
    .map_err(|e| map_err("advance outbox cursor", e))?;
    if updated.rows_affected() == 1 {
        return Ok(true);
    }
    let inserted = sqlx::query(
        "INSERT INTO outbox_cursors (consumer, last_tx_id, last_seq) \
         SELECT $1, $4::text::xid8, $5 WHERE $2::text::xid8 = '0'::xid8 AND $3 = 0 \
         ON CONFLICT (consumer) DO NOTHING",
    )
    .bind(advance.consumer)
    .bind(&from_tx)
    .bind(advance.from.seq)
    .bind(&to_tx)
    .bind(advance.to.seq)
    .execute(conn)
    .await
    .map_err(|e| map_err("advance outbox cursor", e))?;
    Ok(inserted.rows_affected() == 1)
}
