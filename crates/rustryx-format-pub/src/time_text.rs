//! Timestamps as text.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// RFC 3339 text of an instant, or `None` if it cannot be formatted (a year outside the range
/// of the format; not reachable with a real clock).
pub(crate) fn rfc3339(at: OffsetDateTime) -> Option<String> {
    at.format(&Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_instants() {
        let at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        assert_eq!(rfc3339(at).as_deref(), Some("2023-11-14T22:13:20Z"));
    }
}
