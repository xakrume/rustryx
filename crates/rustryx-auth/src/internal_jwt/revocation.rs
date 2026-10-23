//! Revocation list of internal JWT ids (`tid`).
//!
//! A single session ends (logout, admin kill) by listing its `tid`. Revoking all
//! sessions of a user does not use this list: it moves `users.tokens_valid_after`.
//!
//! The list must be shared by every node, so production uses a PostgreSQL
//! implementation. The trait lives here because `rustryx-core` has no port for it
//! yet (reported as a core gap); moving it there changes only the import path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use rustryx_core::StoreError;
use time::OffsetDateTime;

/// Shared set of revoked token ids.
#[async_trait]
pub trait RevokedTokenStore: Send + Sync {
    /// True when `tid` was revoked. Cancel safe.
    async fn is_revoked(&self, tid: &str) -> Result<bool, StoreError>;

    /// Revokes `tid`. `until` is the token's own expiry: after it the entry is
    /// useless and an implementation may purge it. Idempotent. Cancel safe.
    async fn revoke(&self, tid: &str, until: OffsetDateTime) -> Result<(), StoreError>;
}

/// In-process revocation list. Correct for a single node, tests and development;
/// it forgets everything on restart and is invisible to other nodes, so a
/// multi-node deployment must use the shared (PostgreSQL) implementation.
#[derive(Debug, Default)]
pub struct MemRevokedTokens {
    entries: Mutex<HashMap<String, OffsetDateTime>>,
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
impl RevokedTokenStore for MemRevokedTokens {
    async fn is_revoked(&self, tid: &str) -> Result<bool, StoreError> {
        self.check_available()?;
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.contains_key(tid))
    }

    async fn revoke(&self, tid: &str, until: OffsetDateTime) -> Result<(), StoreError> {
        self.check_available()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.entry(tid.to_owned()).or_insert(until);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revoked_ids_are_reported_and_others_are_not() {
        let list = MemRevokedTokens::new();
        assert!(!list.is_revoked("a").await.unwrap());
        list.revoke("a", OffsetDateTime::UNIX_EPOCH).await.unwrap();
        list.revoke("a", OffsetDateTime::UNIX_EPOCH).await.unwrap();
        assert!(list.is_revoked("a").await.unwrap());
        assert!(!list.is_revoked("b").await.unwrap());
    }

    #[tokio::test]
    async fn outage_is_an_error_not_a_negative_answer() {
        let list = MemRevokedTokens::new();
        list.set_unavailable(true);
        assert!(list.is_revoked("a").await.is_err());
        assert!(list.revoke("a", OffsetDateTime::UNIX_EPOCH).await.is_err());
    }
}
