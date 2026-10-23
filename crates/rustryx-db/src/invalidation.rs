//! Cross-node cache invalidation over `LISTEN`/`NOTIFY`.
//!
//! Every RBAC relevant write sends `NOTIFY rustryx_rbac` INSIDE its transaction, so
//! listeners hear about it only after commit and never for a rolled back change. The
//! payload is one of `rbac`, `user:<uuid>`, `reset`, `projects`, `config:<revision>`; an
//! unknown payload decodes to `Reset`, because dropping every cache entry is always safe
//! while ignoring a message is not. That is also what keeps an N-1 node correct when a newer
//! node publishes a payload it has never heard of.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use rustryx_core::{Secret, StoreError};
use sqlx::postgres::PgListener;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::map_err;

/// The notification channel shared by all nodes.
pub const RBAC_CHANNEL: &str = "rustryx_rbac";

const LISTEN_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_BASE: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(10);

/// Text payload of an event.
pub(crate) fn encode_event(event: &InvalidationEvent) -> String {
    match event {
        InvalidationEvent::Rbac => "rbac".to_owned(),
        InvalidationEvent::User(id) => format!("user:{id}"),
        InvalidationEvent::Projects => "projects".to_owned(),
        InvalidationEvent::Config(revision) => format!("config:{revision}"),
        // `Reset` and any variant added to the non exhaustive enum later: dropping every
        // cache is the one message that is always safe.
        _ => "reset".to_owned(),
    }
}

/// Event of a payload; anything not understood becomes `Reset`.
pub(crate) fn decode_event(payload: &str) -> InvalidationEvent {
    if payload == "rbac" {
        return InvalidationEvent::Rbac;
    }
    if payload == "projects" {
        return InvalidationEvent::Projects;
    }
    if let Some(revision) = payload
        .strip_prefix("config:")
        .and_then(|s| s.parse::<i64>().ok())
    {
        return InvalidationEvent::Config(revision);
    }
    if let Some(id) = payload
        .strip_prefix("user:")
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        return InvalidationEvent::User(id);
    }
    InvalidationEvent::Reset
}

/// Sends the event on `conn`. Inside a transaction the notification is queued until commit.
pub(crate) async fn send(
    conn: &mut PgConnection,
    event: &InvalidationEvent,
) -> Result<(), StoreError> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(RBAC_CHANNEL)
        .bind(encode_event(event))
        .execute(conn)
        .await
        .map_err(|e| map_err("send invalidation", e))?;
    Ok(())
}

/// [`InvalidationBus`] on `PostgreSQL`.
///
/// Publishing goes through the pool (one statement, safe behind a transaction-mode pooler).
/// Listening needs a session that lives as long as the subscription, so it opens its own
/// connection from `listen_url`, which must be a DIRECT URL: a pooler would deliver the
/// `LISTEN` to one server session and the notifications to another.
#[derive(Debug, Clone)]
pub struct PgInvalidationBus {
    pool: PgPool,
    listen_url: Secret,
}

impl PgInvalidationBus {
    /// Creates the bus.
    #[must_use]
    pub fn new(pool: PgPool, listen_url: Secret) -> Self {
        Self { pool, listen_url }
    }
}

enum Subscription {
    /// No live listener; `attempts` failed connection attempts so far.
    Down {
        attempts: u32,
    },
    Up(PgListener),
}

async fn open_listener(url: &Secret) -> Result<PgListener, StoreError> {
    let connect = async {
        let mut listener = PgListener::connect(url.expose()).await?;
        listener.listen(RBAC_CHANNEL).await?;
        Ok::<_, sqlx::Error>(listener)
    };
    match tokio::time::timeout(LISTEN_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(listener)) => Ok(listener),
        Ok(Err(e)) => Err(map_err("listen for invalidations", e)),
        Err(_) => Err(StoreError::Unavailable(
            "listen connect timed out".to_owned(),
        )),
    }
}

fn reconnect_delay(attempts: u32) -> Duration {
    RECONNECT_BASE
        .saturating_mul(2_u32.saturating_pow(attempts.min(8)))
        .min(RECONNECT_MAX)
}

#[async_trait]
impl InvalidationBus for PgInvalidationBus {
    async fn publish(&self, event: InvalidationEvent) -> Result<(), StoreError> {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(RBAC_CHANNEL)
            .bind(encode_event(&event))
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("publish invalidation", e))?;
        Ok(())
    }

    /// The stream is lazy: the connection is opened by the first poll, and `Reset` is the
    /// first item after EVERY (re)connection, because notifications sent while the session
    /// was down are gone for good (`LISTEN` has no replay). The `Reset` is yielded only after
    /// `LISTEN` succeeded, so no change can fall between "caches dropped" and "listening".
    /// The stream is pull based and holds no buffer of its own, so a slow consumer cannot
    /// make it lose events silently; a lost session shows up as the next `Reset`.
    fn subscribe(&self) -> BoxStream<'static, InvalidationEvent> {
        let url = self.listen_url.clone();
        Box::pin(futures::stream::unfold(
            Subscription::Down { attempts: 0 },
            move |mut state| {
                let url = url.clone();
                async move {
                    loop {
                        match state {
                            Subscription::Down { attempts } => {
                                if attempts > 0 {
                                    tokio::time::sleep(reconnect_delay(attempts)).await;
                                }
                                match open_listener(&url).await {
                                    Ok(listener) => {
                                        return Some((
                                            InvalidationEvent::Reset,
                                            Subscription::Up(listener),
                                        ));
                                    }
                                    Err(err) => {
                                        tracing::warn!(error = %err, attempts, "invalidation listener cannot connect");
                                        state = Subscription::Down {
                                            attempts: attempts + 1,
                                        };
                                    }
                                }
                            }
                            Subscription::Up(mut listener) => {
                                // `try_recv`, not `recv`: `recv` reconnects silently and
                                // hides the gap, `try_recv` reports it as `None`.
                                if let Ok(Some(notification)) = listener.try_recv().await {
                                    let event = decode_event(notification.payload());
                                    return Some((event, Subscription::Up(listener)));
                                }
                                tracing::warn!("invalidation listener lost its connection");
                                state = Subscription::Down { attempts: 0 };
                            }
                        }
                    }
                }
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip() {
        let id = Uuid::now_v7();
        for event in [
            InvalidationEvent::Rbac,
            InvalidationEvent::User(id),
            InvalidationEvent::Reset,
            InvalidationEvent::Projects,
            InvalidationEvent::Config(0),
            InvalidationEvent::Config(i64::MAX),
        ] {
            assert_eq!(decode_event(&encode_event(&event)), event);
        }
    }

    #[test]
    fn unknown_payload_resets_everything() {
        for payload in [
            "",
            "rbac2",
            "user:not-a-uuid",
            "user:",
            "whatever",
            "projects:1",
            "config:",
            "config:abc",
            "config:99999999999999999999",
        ] {
            assert_eq!(decode_event(payload), InvalidationEvent::Reset, "{payload}");
        }
    }

    #[test]
    fn reconnect_delay_grows_and_is_capped() {
        assert_eq!(reconnect_delay(1), Duration::from_millis(500));
        assert_eq!(reconnect_delay(3), Duration::from_secs(2));
        assert_eq!(reconnect_delay(50), RECONNECT_MAX);
    }
}
