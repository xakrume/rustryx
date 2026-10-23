//! In-process revocation list of internal JWT ids (`tid`), a test double and single node
//! stand-in for the shared `RevokedTokenRepo` port.
//!
//! A single session ends (logout, admin kill) by listing its `tid`. Revoking all sessions of a
//! user does not use this list: it moves `users.tokens_valid_after`.
//!
//! Production uses the PostgreSQL implementation of `rustryx_core::ports::RevokedTokenRepo`
//! (table `revoked_tokens`), which every node shares. This list is per process: it forgets
//! everything on restart and is invisible to other nodes, so a multi node deployment that used
//! it would let a revoked session live on the other nodes. It exists for the tests of this
//! crate and of crates that need a revocation list without a database.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ports::RevokedTokenRepo;
use time::OffsetDateTime;
use uuid::Uuid;

/// In-process revocation list. See the module documentation for why it is not for production.
#[derive(Debug, Default)]
pub struct MemRevokedTokens {
    entries: Mutex<HashMap<Uuid, OffsetDateTime>>,
    unavailable: AtomicBool,
}

impl MemRevokedTokens {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every call fail with `StoreError::Unavailable` (drives fail-closed tests).
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Number of rows held, expired ones included (tests of the purge).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// True when no row is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check_available(&self) -> Result<(), StoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable(
                "revocation list marked unavailable".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl RevokedTokenRepo for MemRevokedTokens {
    async fn revoke(&self, tid: Uuid, expires_at: OffsetDateTime) -> Result<(), StoreError> {
        self.check_available()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        // Idempotent, and the first expiry stays: a repeated revoke must not shorten a row.
        entries.entry(tid).or_insert(expires_at);
        Ok(())
    }

    async fn is_revoked(&self, tid: Uuid) -> Result<bool, StoreError> {
        self.check_available()?;
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.contains_key(&tid))
    }

    async fn purge_expired(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        self.check_available()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let before = entries.len();
        entries.retain(|_, expires_at| *expires_at >= now);
        Ok(u64::try_from(before - entries.len()).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    fn tid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[tokio::test]
    async fn revoked_ids_are_reported_and_others_are_not() {
        let list = MemRevokedTokens::new();
        assert!(list.is_empty());
        assert!(!list.is_revoked(tid(1)).await.unwrap());
        list.revoke(tid(1), OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap();
        list.revoke(tid(1), OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap();
        assert!(list.is_revoked(tid(1)).await.unwrap());
        assert!(!list.is_revoked(tid(2)).await.unwrap());
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn outage_is_an_error_not_a_negative_answer() {
        let list = MemRevokedTokens::new();
        list.set_unavailable(true);
        assert!(list.is_revoked(tid(1)).await.is_err());
        assert!(
            list.revoke(tid(1), OffsetDateTime::UNIX_EPOCH)
                .await
                .is_err()
        );
        assert!(
            list.purge_expired(OffsetDateTime::UNIX_EPOCH)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn purge_removes_only_rows_whose_token_has_expired() {
        let list = MemRevokedTokens::new();
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        list.revoke(tid(1), now - Duration::seconds(1))
            .await
            .unwrap();
        list.revoke(tid(2), now).await.unwrap();
        list.revoke(tid(3), now + Duration::hours(1)).await.unwrap();
        assert_eq!(list.purge_expired(now).await.unwrap(), 1);
        assert!(!list.is_revoked(tid(1)).await.unwrap());
        assert!(list.is_revoked(tid(2)).await.unwrap());
        assert!(list.is_revoked(tid(3)).await.unwrap());
    }

    #[tokio::test]
    async fn must_not_shorten_a_revocation_by_revoking_again() {
        let list = MemRevokedTokens::new();
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        list.revoke(tid(1), now + Duration::hours(1)).await.unwrap();
        list.revoke(tid(1), now - Duration::hours(1)).await.unwrap();
        assert_eq!(list.purge_expired(now).await.unwrap(), 0);
        assert!(list.is_revoked(tid(1)).await.unwrap());
    }
}
