//! Write budget: how many bytes one upload (or cache fill) may still add.
//!
//! The budget is the smallest of the blob size cap, the room left in the repository
//! quota and the room left in the blob store quota. It is used twice, which is how
//! the spec's "by declared size before, by actual size after" is met: the declared
//! length is compared with it before the body is read, and the same number is the
//! byte limit of the sink, so the actual size is enforced while the bytes stream in
//! and nothing over the limit is ever written.
//!
//! Concurrent writers each see the same usage and may together overshoot by their
//! combined size. Quotas are a soft capacity guard (SRE-31 raises an alert at 80%),
//! not an accounting lock.

use crate::context::FormatContext;
use crate::error::FormatError;

/// What limited the budget; selects the error when the limit is crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LimitedBy {
    /// The global blob size cap (`Limits::max_blob_bytes`): 413.
    MaxBlob,
    /// A repository or blob store quota: 507.
    Quota,
}

/// Bytes that may still be written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    pub(crate) bytes: u64,
    pub(crate) limited_by: LimitedBy,
    pub(crate) max_blob_bytes: u64,
}

impl Budget {
    /// The error for crossing the budget.
    pub(crate) fn exceeded(&self) -> FormatError {
        match self.limited_by {
            LimitedBy::MaxBlob => FormatError::PayloadTooLarge {
                limit: self.max_blob_bytes,
            },
            LimitedBy::Quota => FormatError::QuotaExceeded,
        }
    }

    /// Rejects a declared size that cannot fit.
    pub(crate) fn check_declared(&self, declared: Option<u64>) -> Result<(), FormatError> {
        match declared {
            Some(size) if size > self.bytes => Err(self.exceeded()),
            _ => Ok(()),
        }
    }
}

/// `quota - used + credit`, never below zero.
fn room(quota: u64, used: u64, credit: u64) -> u64 {
    quota.saturating_add(credit).saturating_sub(used)
}

/// Computes the budget for a write to the context's repository.
///
/// `replaced_size` is the size of the asset a re-publish will replace: that space
/// is returned to the repository when the old blob loses its reference.
pub(crate) async fn budget(cx: &FormatContext, replaced_size: u64) -> Result<Budget, FormatError> {
    let repository = cx.repository();
    let max_blob_bytes = cx.limits().max_blob_bytes;
    let mut result = Budget {
        bytes: max_blob_bytes,
        limited_by: LimitedBy::MaxBlob,
        max_blob_bytes,
    };
    if let Some(quota) = repository.quota_bytes {
        let used = cx
            .ports()
            .blobs
            .used_bytes_by_repository(repository.id)
            .await?;
        let available = room(quota, used, replaced_size);
        if available < result.bytes {
            result.bytes = available;
            result.limited_by = LimitedBy::Quota;
        }
    }
    if let Some(definition) = cx
        .ports()
        .blob_store_defs
        .get(repository.blob_store_id)
        .await?
        && let Some(quota) = definition.quota_bytes
    {
        // Replaced blobs still occupy the store until GC removes them, so there is no credit here.
        let used = cx.ports().blobs.used_bytes_by_store(definition.id).await?;
        let available = room(quota, used, 0);
        if available < result.bytes {
            result.bytes = available;
            result.limited_by = LimitedBy::Quota;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_never_underflows() {
        assert_eq!(room(100, 40, 0), 60);
        assert_eq!(room(100, 140, 0), 0);
        assert_eq!(room(100, 140, 50), 10);
        assert_eq!(room(u64::MAX, 1, u64::MAX), u64::MAX - 1);
    }

    #[test]
    fn exceeded_error_depends_on_the_limiting_factor() {
        let by_cap = Budget {
            bytes: 10,
            limited_by: LimitedBy::MaxBlob,
            max_blob_bytes: 10,
        };
        assert!(matches!(
            by_cap.exceeded(),
            FormatError::PayloadTooLarge { limit: 10 }
        ));
        let by_quota = Budget {
            limited_by: LimitedBy::Quota,
            ..by_cap
        };
        assert!(matches!(by_quota.exceeded(), FormatError::QuotaExceeded));
        assert!(by_cap.check_declared(Some(10)).is_ok());
        assert!(by_cap.check_declared(None).is_ok());
        assert!(by_cap.check_declared(Some(11)).is_err());
    }
}
