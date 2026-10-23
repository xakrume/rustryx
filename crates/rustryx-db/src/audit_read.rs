//! [`AuditReadRepo`]: the read side of the audit log (viewer, project log view, NDJSON export).
//!
//! The sink stays write only. Both reads filter in SQL, so a filter is a real boundary: a
//! query scoped to a project returns that project's rows and nothing else, and a token only
//! moves the window (db.md section 5).
//!
//! Order. `query` pages newest first and `export` streams oldest first, both by
//! `(event_time, id)`: the time orders by when the event happened, the `UUIDv7` id breaks ties
//! of one microsecond deterministically, and the primary key `(id, event_time)` plus
//! `ix_audit_events_time` serve the keyset predicate.

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::stream::BoxStream;
use rustryx_core::StoreError;
use rustryx_core::audit::{ActorKind, AuditEvent, AuditKind};
use rustryx_core::ids::{ProjectId, UserId};
use rustryx_core::ports::{AuditQuery, AuditReadRepo, Page, PageRequest, StoredAuditEvent};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cursor;
use crate::error::map_err;
use crate::repos::escape_like;

const KIND: &str = "audit";

/// Rows per round trip of an export. One batch is all that is buffered, so a slow consumer
/// holds a few hundred rows in memory and no connection at all between batches.
const EXPORT_BATCH: usize = 500;

const COLUMNS: &str = "id, event_time, kind, actor_id, actor_name, client_ip, request_id, \
                       action, target, outcome, detail, actor_kind, project_id";

/// Audit log reads.
#[derive(Debug, Clone)]
pub struct PgAuditRead {
    pool: PgPool,
}

impl PgAuditRead {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    event_time: OffsetDateTime,
    kind: String,
    actor_id: Option<Uuid>,
    actor_name: String,
    client_ip: Option<String>,
    request_id: Option<String>,
    action: String,
    target: Option<String>,
    outcome: String,
    detail: serde_json::Value,
    actor_kind: String,
    project_id: Option<Uuid>,
}

impl Row {
    /// A kind or an actor kind this build does not know is an error and not a skipped row: an
    /// export that silently dropped rows would be an incomplete audit trail. It only happens
    /// when a newer node wrote a kind during a rolling update.
    fn into_stored(self) -> Result<StoredAuditEvent, StoreError> {
        let corrupt = || crate::error::backend("read audit event", "unknown audit kind");
        let kind = AuditKind::parse(&self.kind).ok_or_else(corrupt)?;
        let actor_kind = ActorKind::parse(&self.actor_kind).ok_or_else(corrupt)?;
        let mut event = AuditEvent::new(kind, self.actor_name, self.action, self.outcome);
        event.at = self.event_time;
        event.actor_id = self.actor_id.map(UserId::from_uuid);
        event.client_ip = self.client_ip;
        event.request_id = self.request_id;
        event.target = self.target;
        event.detail = self.detail;
        event.actor_kind = actor_kind;
        event.project_id = self.project_id.map(ProjectId::from_uuid);
        Ok(StoredAuditEvent { id: self.id, event })
    }
}

/// The keyset position: the `(event_time, id)` of the last row handed out.
#[derive(Debug, Clone, Copy)]
struct Position {
    at: OffsetDateTime,
    id: Uuid,
}

impl Position {
    fn of(stored: &StoredAuditEvent) -> Self {
        Self {
            at: stored.event.at,
            id: stored.id,
        }
    }

    /// `<microseconds since the epoch>:<uuid>`. Microseconds are the precision of
    /// `timestamptz`, so the position round trips exactly.
    fn encode(self) -> String {
        let micros = self.at.unix_timestamp_nanos() / 1000;
        cursor_key(micros, self.id)
    }

    fn decode(key: &str) -> Result<Self, StoreError> {
        let (micros, id) = key.split_once(':').ok_or_else(cursor::invalid_token)?;
        let micros: i128 = micros.parse().map_err(|_| cursor::invalid_token())?;
        let at = OffsetDateTime::from_unix_timestamp_nanos(
            micros.checked_mul(1000).ok_or_else(cursor::invalid_token)?,
        )
        .map_err(|_| cursor::invalid_token())?;
        let id = Uuid::parse_str(id).map_err(|_| cursor::invalid_token())?;
        Ok(Self { at, id })
    }
}

fn cursor_key(micros: i128, id: Uuid) -> String {
    format!("{micros}:{id}")
}

/// Appends the `WHERE` clause of `query`. Every value is a bind parameter; the `LIKE` of the
/// action prefix escapes its wildcards, so a prefix is matched literally.
fn push_filters(builder: &mut QueryBuilder<Postgres>, query: &AuditQuery) {
    builder.push(" WHERE TRUE");
    if let Some(from) = query.from {
        builder.push(" AND event_time >= ").push_bind(from);
    }
    if let Some(to) = query.to {
        builder.push(" AND event_time < ").push_bind(to);
    }
    if !query.kinds.is_empty() {
        let kinds: Vec<&'static str> = query.kinds.iter().map(|k| k.as_str()).collect();
        builder.push(" AND kind = ANY(").push_bind(kinds).push(")");
    }
    if let Some(actor_id) = query.actor_id {
        builder
            .push(" AND actor_id = ")
            .push_bind(actor_id.as_uuid());
    }
    if let Some(actor_name) = &query.actor_name {
        builder
            .push(" AND actor_name = ")
            .push_bind(actor_name.clone());
    }
    if let Some(prefix) = &query.action_prefix {
        builder
            .push(" AND action LIKE ")
            .push_bind(format!("{}%", escape_like(prefix)))
            .push(" ESCAPE '\\'");
    }
    if let Some(target) = &query.target {
        builder.push(" AND target = ").push_bind(target.clone());
    }
    if let Some(project_id) = query.project_id {
        builder
            .push(" AND project_id = ")
            .push_bind(project_id.as_uuid());
    }
    if let Some(outcome) = &query.outcome {
        builder.push(" AND outcome = ").push_bind(outcome.clone());
    }
}

/// Reads up to `limit` rows after `after` in the given direction.
async fn fetch(
    pool: &PgPool,
    query: &AuditQuery,
    after: Option<Position>,
    newest_first: bool,
    limit: i64,
) -> Result<Vec<StoredAuditEvent>, StoreError> {
    let mut builder = QueryBuilder::<Postgres>::new("SELECT ");
    builder.push(COLUMNS).push(" FROM audit_events");
    push_filters(&mut builder, query);
    if let Some(position) = after {
        builder
            .push(if newest_first {
                " AND (event_time, id) < ("
            } else {
                " AND (event_time, id) > ("
            })
            .push_bind(position.at)
            .push(", ")
            .push_bind(position.id)
            .push(")");
    }
    builder.push(if newest_first {
        " ORDER BY event_time DESC, id DESC LIMIT "
    } else {
        " ORDER BY event_time ASC, id ASC LIMIT "
    });
    builder.push_bind(limit);
    builder
        .build_query_as::<Row>()
        .fetch_all(pool)
        .await
        .map_err(|e| map_err("read audit events", e))?
        .into_iter()
        .map(Row::into_stored)
        .collect()
}

struct ExportState {
    pool: PgPool,
    query: AuditQuery,
    after: Option<Position>,
    buffer: VecDeque<StoredAuditEvent>,
    finished: bool,
}

#[async_trait]
impl AuditReadRepo for PgAuditRead {
    async fn query(
        &self,
        query: &AuditQuery,
        page: &PageRequest,
    ) -> Result<Page<StoredAuditEvent>, StoreError> {
        let (limit, fetch_n) = cursor::fetch_limit(page);
        let after = page
            .after
            .as_deref()
            .map(|token| cursor::decode(KIND, token).and_then(|key| Position::decode(&key)))
            .transpose()?;
        let rows = fetch(&self.pool, query, after, true, fetch_n).await?;
        Ok(cursor::finish(rows, limit, KIND, |stored| {
            Position::of(stored).encode()
        }))
    }

    /// Oldest first, in keyset batches. A batch is a separate statement, so the stream holds no
    /// connection while the consumer is slow (a long NDJSON download would otherwise pin a
    /// pooled connection for its whole duration), and dropping the stream ends the work at
    /// once. The price is that the export is not one snapshot: an event inserted with an `at`
    /// before the position already handed out is not seen. Events are stamped when they
    /// happen and written at once, so that window is the clock skew between nodes; an export
    /// is a report of the past, not a replication feed.
    ///
    /// The first error ends the stream after it was yielded.
    fn export(
        &self,
        query: &AuditQuery,
    ) -> BoxStream<'static, Result<StoredAuditEvent, StoreError>> {
        let state = ExportState {
            pool: self.pool.clone(),
            query: query.clone(),
            after: None,
            buffer: VecDeque::new(),
            finished: false,
        };
        Box::pin(futures::stream::unfold(state, |mut state| async move {
            loop {
                if let Some(next) = state.buffer.pop_front() {
                    return Some((Ok(next), state));
                }
                if state.finished {
                    return None;
                }
                let batch = i64::try_from(EXPORT_BATCH).unwrap_or(500);
                match fetch(&state.pool, &state.query, state.after, false, batch).await {
                    Ok(rows) => {
                        state.finished = rows.len() < EXPORT_BATCH;
                        state.after = rows.last().map(Position::of).or(state.after);
                        state.buffer.extend(rows);
                    }
                    Err(err) => {
                        state.finished = true;
                        return Some((Err(err), state));
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_round_trips_at_microsecond_precision() {
        let id = Uuid::now_v7();
        let at = OffsetDateTime::from_unix_timestamp_nanos(1_790_000_000_123_456_000).unwrap();
        let decoded = Position::decode(&Position { at, id }.encode()).unwrap();
        assert_eq!(decoded.at, at);
        assert_eq!(decoded.id, id);
    }

    #[test]
    fn malformed_positions_are_invalid_input() {
        let id = Uuid::now_v7();
        for key in [
            String::new(),
            "12345".to_owned(),
            format!("abc:{id}"),
            "123:not-a-uuid".to_owned(),
            format!("{}:{id}", i128::MAX),
            format!("{}:{id}", i128::MIN),
        ] {
            assert!(
                matches!(Position::decode(&key), Err(StoreError::Invalid(_))),
                "{key}"
            );
        }
    }
}
