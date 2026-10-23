//! The event outbox.
//!
//! State changing methods write their events through [`State::push_events`] under the lock
//! that makes the change, which is how "an event exists if and only if the change committed"
//! holds here. [`OutboxRepo`] is the consumer side.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft, OutboxBatch, OutboxCursor, OutboxEvent};
use rustryx_core::ids::EventId;
use rustryx_core::ports::OutboxRepo;
use time::OffsetDateTime;

use super::MemStore;
use super::state::State;

/// Checks every draft before anything is written, so one oversized event leaves no sibling
/// behind (the database does it with the transaction).
pub(crate) fn validate_drafts(events: &[EventDraft]) -> Result<(), StoreError> {
    events.iter().try_for_each(EventDraft::validate)
}

impl State {
    /// Appends `events` as one "transaction": one `tx_id`, consecutive sequence numbers.
    /// Does nothing for an empty slice and does not consume a transaction id then.
    pub(crate) fn push_events(&mut self, events: &[EventDraft], now: OffsetDateTime) {
        if events.is_empty() {
            return;
        }
        self.tx_seq += 1;
        let tx_id = self.tx_seq;
        for draft in events {
            self.outbox_seq += 1;
            let mut event = OutboxEvent::new(
                EventId::generate(),
                self.outbox_seq,
                now,
                draft.kind,
                draft.actor.clone(),
                draft.data.clone(),
            );
            event.project_id = draft.project_id;
            event.repository_id = draft.repository_id;
            event.request_id.clone_from(&draft.request_id);
            self.outbox.push((tx_id, event));
        }
    }

    fn cursor_of(&self, consumer: &str) -> OutboxCursor {
        self.outbox_cursors
            .get(consumer)
            .copied()
            .unwrap_or_default()
    }
}

#[async_trait]
impl OutboxRepo for MemStore {
    async fn enqueue(&self, events: &[EventDraft]) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        self.tables()?.push_events(events, now);
        Ok(())
    }

    async fn read_batch(&self, consumer: &str, limit: u32) -> Result<OutboxBatch, StoreError> {
        let state = self.tables()?;
        let from = state.cursor_of(consumer);
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        // Every transaction of the double is finished the moment its method returns, so the
        // "below the snapshot horizon" condition of the real relay is always true here.
        let mut rows: Vec<&(u64, OutboxEvent)> = state
            .outbox
            .iter()
            .filter(|(tx_id, event)| (*tx_id, event.seq) > (from.tx_id, from.seq))
            .collect();
        rows.sort_by_key(|(tx_id, event)| (*tx_id, event.seq));
        rows.truncate(limit);
        let to = rows.last().map_or(from, |(tx_id, event)| OutboxCursor {
            tx_id: *tx_id,
            seq: event.seq,
        });
        Ok(OutboxBatch {
            events: rows.into_iter().map(|(_, event)| event.clone()).collect(),
            from,
            to,
        })
    }

    async fn advance(&self, advance: &CursorAdvance) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        // Compare and set: the caller that lost the race reads again from the new cursor.
        if state.cursor_of(advance.consumer) != advance.from {
            return Ok(false);
        }
        state
            .outbox_cursors
            .insert(advance.consumer.to_owned(), advance.to);
        Ok(true)
    }

    async fn lag(&self, consumer: &str) -> Result<Duration, StoreError> {
        let now = self.now();
        let state = self.tables()?;
        let cursor = state.cursor_of(consumer);
        let oldest = state
            .outbox
            .iter()
            .filter(|(tx_id, event)| (*tx_id, event.seq) > (cursor.tx_id, cursor.seq))
            .map(|(_, event)| event.occurred_at)
            .min();
        // A clock that stands before the event (a test that sets it back) reads as no lag.
        Ok(oldest.map_or(Duration::ZERO, |at| {
            Duration::try_from(now - at).unwrap_or(Duration::ZERO)
        }))
    }

    async fn purge(&self, older_than: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        // Never past the slowest cursor. With no cursor at all no consumer has started, and
        // dropping events then would lose them for every consumer that starts later.
        let Some(slowest) = state.outbox_cursors.values().min().copied() else {
            return Ok(0);
        };
        let before = state.outbox.len();
        state.outbox.retain(|(tx_id, event)| {
            let handled = (*tx_id, event.seq) <= (slowest.tx_id, slowest.seq);
            !(handled && event.occurred_at < older_than)
        });
        Ok(u64::try_from(before - state.outbox.len()).unwrap_or(u64::MAX))
    }
}
