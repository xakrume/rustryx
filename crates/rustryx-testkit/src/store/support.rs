//! Small rules every versioned entity of the store shares.

use rustryx_core::error::StoreError;

use super::conflict;

/// Optimistic concurrency: the write is accepted only against the version the caller read,
/// and moves the row to the next one. A stale version is `Conflict`, which the API turns
/// into 412 or 409 and a reload of the form.
pub(crate) fn bump_version(current: i64, expected: i64) -> Result<i64, StoreError> {
    if current != expected {
        return Err(conflict(format!(
            "stale version: expected {expected}, found {current}"
        )));
    }
    Ok(current.saturating_add(1))
}

/// Longest log message a log table keeps, in characters (the column limit).
pub(crate) const MAX_LOG_MESSAGE_CHARS: usize = 1024;

/// Shortens a log message to the column limit, so a long line is stored cut instead of failing
/// the whole append.
pub(crate) fn clip_message(message: &str) -> String {
    message.chars().take(MAX_LOG_MESSAGE_CHARS).collect()
}

/// Lines of one log after `after_seq`, in `seq` order, at most `limit`.
pub(crate) fn read_lines(
    lines: &[rustryx_core::tasks::TaskLogLine],
    after_seq: i32,
    limit: u32,
) -> Vec<rustryx_core::tasks::TaskLogLine> {
    let mut selected: Vec<rustryx_core::tasks::TaskLogLine> = lines
        .iter()
        .filter(|line| line.seq > after_seq)
        .cloned()
        .collect();
    selected.sort_by_key(|line| line.seq);
    selected.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    selected
}
