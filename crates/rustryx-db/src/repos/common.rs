//! Small helpers shared by the repositories.

use rustryx_core::StoreError;
use rustryx_core::ports::InvalidationEvent;
use sqlx::PgConnection;

use crate::error::backend;
use crate::invalidation;

/// A value read from the database that the domain types reject. The constraints of the
/// schema make this unreachable for rows written by this crate; it exists so that a row
/// written by hand cannot panic a node.
pub(crate) fn corrupt(context: &'static str) -> StoreError {
    backend(context, "stored row does not match the domain model")
}

/// `u64` to the `bigint` the schema uses. Quotas and sizes above `i64::MAX` do not exist
/// on any real disk; rejecting them is cheaper than a wrapping cast. The input is malformed,
/// not in conflict with a stored state, hence `Invalid`.
pub(crate) fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Invalid("numeric value out of range".to_owned()))
}

/// `bigint` (checked non-negative by the schema) to `u64`.
pub(crate) fn to_u64(value: i64, context: &'static str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt(context))
}

/// Escapes `\`, `%` and `_` so that user text is matched literally by `LIKE ... ESCAPE '\'`.
pub(crate) fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Queues an RBAC invalidation inside the open transaction.
pub(crate) async fn notify(
    conn: &mut PgConnection,
    event: &InvalidationEvent,
) -> Result<(), StoreError> {
    invalidation::send(conn, event).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(escape_like("plain/path"), "plain/path");
    }

    #[test]
    fn numeric_range_is_checked() {
        assert!(matches!(to_i64(u64::MAX), Err(StoreError::Invalid(_))));
        assert_eq!(to_i64(7).unwrap(), 7);
        assert!(to_u64(-1, "t").is_err());
    }
}
