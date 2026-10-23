//! Blob store quota helpers (SRE-31, architecture 1.4: "quota is checked before the
//! write by the declared size and after the write by the actual size").
//!
//! These are pure functions. The caller owns the numbers: `used_bytes` comes from
//! `BlobRepo::used_bytes_by_store` (distinct blobs), `quota` from
//! `BlobStoreDef::quota_bytes`. Two concurrent uploads can both pass the pre-check
//! and together overshoot, so the quota is a soft ceiling by at most the number of
//! in-flight uploads times their size. That is accepted because the alternative is a
//! global lock on the write path.

use rustryx_core::DomainError;

/// A write would push a store over its quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "blob store quota exceeded: {used_bytes} of {quota_bytes} bytes used, \
     {requested_bytes} more requested"
)]
pub struct QuotaExceeded {
    /// Configured quota in bytes.
    pub quota_bytes: u64,
    /// Bytes already stored (not counting the write being checked).
    pub used_bytes: u64,
    /// Size of the write being checked.
    pub requested_bytes: u64,
}

impl From<QuotaExceeded> for DomainError {
    fn from(_: QuotaExceeded) -> Self {
        Self::QuotaExceeded
    }
}

/// Checks a write before any byte is accepted.
///
/// `declared_size` is the announced size (`Content-Length`) when there is one.
/// Without a declared size only the already-full case can be rejected here; the
/// real size is enforced by [`check_quota_after_write`]. Reaching the quota exactly
/// is allowed. `quota == None` means unlimited.
pub fn check_quota(
    declared_size: Option<u64>,
    used_bytes: u64,
    quota: Option<u64>,
) -> Result<(), QuotaExceeded> {
    let Some(quota_bytes) = quota else {
        return Ok(());
    };
    let exceeded = match declared_size {
        Some(size) => used_bytes
            .checked_add(size)
            .is_none_or(|total| total > quota_bytes),
        None => used_bytes >= quota_bytes,
    };
    if exceeded {
        return Err(QuotaExceeded {
            quota_bytes,
            used_bytes,
            requested_bytes: declared_size.unwrap_or(0),
        });
    }
    Ok(())
}

/// Checks a finished write by its actual size. `used_bytes` is the usage from
/// before this write. On error the caller deletes the blob it just committed (and
/// does not publish metadata for it).
pub fn check_quota_after_write(
    actual_size: u64,
    used_bytes: u64,
    quota: Option<u64>,
) -> Result<(), QuotaExceeded> {
    check_quota(Some(actual_size), used_bytes, quota)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_quota_accepts_anything() {
        assert!(check_quota(Some(u64::MAX), u64::MAX, None).is_ok());
        assert!(check_quota(None, u64::MAX, None).is_ok());
    }

    #[test]
    fn declared_size_may_reach_the_quota_exactly() {
        assert!(check_quota(Some(40), 60, Some(100)).is_ok());
        assert!(check_quota(Some(0), 100, Some(100)).is_ok());
    }

    #[test]
    fn must_not_accept_a_declared_size_over_the_quota() {
        let err = check_quota(Some(41), 60, Some(100)).unwrap_err();
        assert_eq!(
            err,
            QuotaExceeded {
                quota_bytes: 100,
                used_bytes: 60,
                requested_bytes: 41
            }
        );
        assert!(matches!(DomainError::from(err), DomainError::QuotaExceeded));
    }

    #[test]
    fn must_not_overflow_when_adding_sizes() {
        assert!(check_quota(Some(u64::MAX), 1, Some(u64::MAX)).is_err());
    }

    #[test]
    fn unknown_size_is_rejected_only_when_the_store_is_already_full() {
        assert!(check_quota(None, 99, Some(100)).is_ok());
        assert!(check_quota(None, 100, Some(100)).is_err());
        assert!(check_quota(None, 0, Some(0)).is_err());
    }

    #[test]
    fn must_not_accept_an_actual_size_over_the_quota_after_the_write() {
        assert!(check_quota_after_write(40, 60, Some(100)).is_ok());
        assert!(check_quota_after_write(41, 60, Some(100)).is_err());
        assert!(check_quota_after_write(1, 0, Some(0)).is_err());
    }

    #[test]
    fn message_names_the_numbers() {
        let text = check_quota(Some(5), 98, Some(100)).unwrap_err().to_string();
        assert!(text.contains("98") && text.contains("100") && text.contains('5'));
    }
}
