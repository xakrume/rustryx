//! Conditional requests (RFC 9110 section 13) and byte ranges (section 14).
//!
//! These are pure functions over headers so that hosted downloads, proxy cache hits
//! and group responses all evaluate validators identically.

use http::{HeaderMap, Method, header};
use rustryx_core::model::Checksums;
use rustryx_core::ports::ByteRange;
use time::OffsetDateTime;
use time::macros::format_description;

/// Formats a time as an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`).
#[must_use]
pub fn format_http_date(at: OffsetDateTime) -> String {
    let format = format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    at.to_offset(time::UtcOffset::UTC)
        .format(&format)
        .unwrap_or_default()
}

/// Parses an HTTP date in the preferred (IMF-fixdate) format.
#[must_use]
pub fn parse_http_date(value: &str) -> Option<OffsetDateTime> {
    let format = format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    time::PrimitiveDateTime::parse(value.trim(), &format)
        .ok()
        .map(time::PrimitiveDateTime::assume_utc)
}

/// Strong entity tag derived from the SHA-256 of the content.
///
/// A content hash gives a validator that changes exactly when the bytes change,
/// including when a path is re-published with different content.
#[must_use]
pub fn etag_from_checksums(checksums: &Checksums) -> String {
    format!("\"{}\"", checksums.sha256)
}

/// Outcome of evaluating `If-*` preconditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conditional {
    /// Serve the representation.
    Proceed,
    /// Answer 304: the client's copy is current.
    NotModified,
    /// Answer 412: a precondition failed.
    PreconditionFailed,
}

/// Evaluates `If-Match`, `If-Unmodified-Since`, `If-None-Match` and
/// `If-Modified-Since` in the order of RFC 9110 section 13.2.2.
///
/// `last_modified` is compared at one-second resolution, the resolution of HTTP dates.
#[must_use]
pub fn evaluate_conditional(
    method: &Method,
    headers: &HeaderMap,
    etag: &str,
    last_modified: OffsetDateTime,
) -> Conditional {
    let safe = method == Method::GET || method == Method::HEAD;
    let modified_secs = last_modified.unix_timestamp();

    if let Some(value) = header_text(headers, &header::IF_MATCH) {
        if !etag_list_matches(value, etag, true) {
            return Conditional::PreconditionFailed;
        }
    } else if let Some(since) = date_header(headers, &header::IF_UNMODIFIED_SINCE)
        && modified_secs > since.unix_timestamp()
    {
        return Conditional::PreconditionFailed;
    }

    if let Some(value) = header_text(headers, &header::IF_NONE_MATCH) {
        if etag_list_matches(value, etag, false) {
            return if safe {
                Conditional::NotModified
            } else {
                Conditional::PreconditionFailed
            };
        }
    } else if safe
        && let Some(since) = date_header(headers, &header::IF_MODIFIED_SINCE)
        && modified_secs <= since.unix_timestamp()
    {
        return Conditional::NotModified;
    }
    Conditional::Proceed
}

/// True when the request's `If-None-Match` matches `etag` (weak comparison, `*` matches).
///
/// For responses that have no meaningful modification date (a merged group index),
/// where the full [`evaluate_conditional`] would let `If-Modified-Since` produce a
/// wrong 304.
#[must_use]
pub fn if_none_match_hits(headers: &HeaderMap, etag: &str) -> bool {
    header_text(headers, &header::IF_NONE_MATCH)
        .is_some_and(|list| etag_list_matches(list, etag, false))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &header::HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn date_header(headers: &HeaderMap, name: &header::HeaderName) -> Option<OffsetDateTime> {
    header_text(headers, name).and_then(parse_http_date)
}

/// True when `list` (`*` or comma separated entity tags) contains `etag`.
/// `strong` selects strong comparison (`If-Match`, `If-Range`); otherwise weak
/// comparison ignores the `W/` prefix (`If-None-Match`).
fn etag_list_matches(list: &str, etag: &str, strong: bool) -> bool {
    let list = list.trim();
    if list == "*" {
        return true;
    }
    list.split(',').map(str::trim).any(|candidate| {
        if strong && candidate.starts_with("W/") {
            return false;
        }
        candidate.trim_start_matches("W/") == etag.trim_start_matches("W/")
    })
}

/// Decision about a `Range` header for a representation of known size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDecision {
    /// No usable range: send the whole representation with 200.
    Full,
    /// Send this inclusive range with 206. `end` is always `Some`.
    Partial(ByteRange),
    /// Answer 416.
    Unsatisfiable,
}

/// Evaluates `Range` (and `If-Range`) against a representation of `total` bytes.
///
/// Only a single `bytes` range is honoured. Multiple ranges and malformed headers
/// are ignored, which RFC 9110 allows, so a client never gets a multipart body
/// we do not produce. A failing `If-Range` also yields a full response.
#[must_use]
pub fn decide_range(
    headers: &HeaderMap,
    total: u64,
    etag: &str,
    last_modified: OffsetDateTime,
) -> RangeDecision {
    let Some(spec) = header_text(headers, &header::RANGE) else {
        return RangeDecision::Full;
    };
    if let Some(validator) = header_text(headers, &header::IF_RANGE) {
        let still_valid = match parse_http_date(validator) {
            Some(date) => date.unix_timestamp() == last_modified.unix_timestamp(),
            None => etag_list_matches(validator, etag, true),
        };
        if !still_valid {
            return RangeDecision::Full;
        }
    }
    parse_single_range(spec, total)
}

fn parse_single_range(spec: &str, total: u64) -> RangeDecision {
    let Some(ranges) = spec.trim().strip_prefix("bytes=") else {
        return RangeDecision::Full;
    };
    if ranges.contains(',') {
        return RangeDecision::Full;
    }
    let Some((first, last)) = ranges.trim().split_once('-') else {
        return RangeDecision::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    let range = match (first.is_empty(), last.is_empty()) {
        (true, true) => return RangeDecision::Full,
        (true, false) => {
            // Suffix range: the last N bytes.
            let Ok(suffix) = last.parse::<u64>() else {
                return RangeDecision::Full;
            };
            if suffix == 0 || total == 0 {
                return RangeDecision::Unsatisfiable;
            }
            (total.saturating_sub(suffix), total - 1)
        }
        (false, _) => {
            let Ok(start) = first.parse::<u64>() else {
                return RangeDecision::Full;
            };
            let end = if last.is_empty() {
                u64::MAX
            } else {
                match last.parse::<u64>() {
                    Ok(end) if end >= start => end,
                    _ => return RangeDecision::Full,
                }
            };
            if start >= total {
                return RangeDecision::Unsatisfiable;
            }
            (start, end.min(total - 1))
        }
    };
    RangeDecision::Partial(ByteRange {
        start: range.0,
        end: Some(range.1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert((*name).clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    #[test]
    fn http_date_round_trips_and_matches_rfc_example() {
        let t = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(t.unix_timestamp(), 784_111_777);
        assert_eq!(format_http_date(t), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert!(parse_http_date("yesterday").is_none());
    }

    #[test]
    fn etag_is_quoted_sha256() {
        let sums = Checksums {
            sha1: "a".into(),
            sha256: "deadbeef".into(),
            md5: "c".into(),
        };
        assert_eq!(etag_from_checksums(&sums), "\"deadbeef\"");
    }

    #[test]
    fn if_none_match_gives_304_for_get_and_412_for_put() {
        let h = headers(&[(&header::IF_NONE_MATCH, "\"x\", W/\"abc\"")]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"abc\"", at(0)),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate_conditional(&Method::HEAD, &h, "\"x\"", at(0)),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate_conditional(&Method::PUT, &h, "\"abc\"", at(0)),
            Conditional::PreconditionFailed
        );
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"other\"", at(0)),
            Conditional::Proceed
        );
        let star = headers(&[(&header::IF_NONE_MATCH, "*")]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &star, "\"any\"", at(0)),
            Conditional::NotModified
        );
    }

    #[test]
    fn if_match_requires_strong_match() {
        let h = headers(&[(&header::IF_MATCH, "\"abc\"")]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"abc\"", at(0)),
            Conditional::Proceed
        );
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"zzz\"", at(0)),
            Conditional::PreconditionFailed
        );
        let weak = headers(&[(&header::IF_MATCH, "W/\"abc\"")]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &weak, "\"abc\"", at(0)),
            Conditional::PreconditionFailed
        );
        let star = headers(&[(&header::IF_MATCH, "*")]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &star, "\"abc\"", at(0)),
            Conditional::Proceed
        );
    }

    #[test]
    fn modified_since_uses_second_resolution() {
        let date = format_http_date(at(1_000));
        let h = headers(&[(&header::IF_MODIFIED_SINCE, &date)]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(1_000)),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(999)),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(1_001)),
            Conditional::Proceed
        );
        // Not applicable to unsafe methods.
        assert_eq!(
            evaluate_conditional(&Method::PUT, &h, "\"e\"", at(0)),
            Conditional::Proceed
        );
    }

    #[test]
    fn unmodified_since_fails_when_changed_later() {
        let date = format_http_date(at(1_000));
        let h = headers(&[(&header::IF_UNMODIFIED_SINCE, &date)]);
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(2_000)),
            Conditional::PreconditionFailed
        );
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(500)),
            Conditional::Proceed
        );
    }

    #[test]
    fn if_none_match_takes_precedence_over_modified_since() {
        let date = format_http_date(at(5_000));
        let h = headers(&[
            (&header::IF_NONE_MATCH, "\"other\""),
            (&header::IF_MODIFIED_SINCE, &date),
        ]);
        // The ETag list does not match, so the date must be ignored entirely.
        assert_eq!(
            evaluate_conditional(&Method::GET, &h, "\"e\"", at(1_000)),
            Conditional::Proceed
        );
    }

    #[test]
    fn if_none_match_hits_ignores_dates() {
        let h = headers(&[(&header::IF_NONE_MATCH, "\"a\", \"b\"")]);
        assert!(if_none_match_hits(&h, "\"b\""));
        assert!(!if_none_match_hits(&h, "\"c\""));
        let date_only = headers(&[(&header::IF_MODIFIED_SINCE, &format_http_date(at(9_999_999)))]);
        assert!(!if_none_match_hits(&date_only, "\"b\""));
        assert!(!if_none_match_hits(&HeaderMap::new(), "\"b\""));
    }

    #[test]
    fn range_forms() {
        let none = HeaderMap::new();
        assert_eq!(
            decide_range(&none, 100, "\"e\"", at(0)),
            RangeDecision::Full
        );
        let cases = [
            (
                "bytes=0-9",
                RangeDecision::Partial(ByteRange {
                    start: 0,
                    end: Some(9),
                }),
            ),
            (
                "bytes=90-",
                RangeDecision::Partial(ByteRange {
                    start: 90,
                    end: Some(99),
                }),
            ),
            (
                "bytes=90-500",
                RangeDecision::Partial(ByteRange {
                    start: 90,
                    end: Some(99),
                }),
            ),
            (
                "bytes=-10",
                RangeDecision::Partial(ByteRange {
                    start: 90,
                    end: Some(99),
                }),
            ),
            (
                "bytes=-500",
                RangeDecision::Partial(ByteRange {
                    start: 0,
                    end: Some(99),
                }),
            ),
            ("bytes=100-", RangeDecision::Unsatisfiable),
            ("bytes=200-300", RangeDecision::Unsatisfiable),
            ("bytes=-0", RangeDecision::Unsatisfiable),
            ("bytes=0-1,5-6", RangeDecision::Full),
            ("bytes=9-3", RangeDecision::Full),
            ("bytes=a-b", RangeDecision::Full),
            ("bytes=-", RangeDecision::Full),
            ("bytes=5", RangeDecision::Full),
            ("items=0-1", RangeDecision::Full),
            ("bytes=x-", RangeDecision::Full),
            ("bytes=-x", RangeDecision::Full),
        ];
        for (spec, expected) in cases {
            let h = headers(&[(&header::RANGE, spec)]);
            assert_eq!(decide_range(&h, 100, "\"e\"", at(0)), expected, "{spec}");
        }
    }

    #[test]
    fn empty_representation_has_no_satisfiable_range() {
        let h = headers(&[(&header::RANGE, "bytes=-5")]);
        assert_eq!(
            decide_range(&h, 0, "\"e\"", at(0)),
            RangeDecision::Unsatisfiable
        );
        let h = headers(&[(&header::RANGE, "bytes=0-")]);
        assert_eq!(
            decide_range(&h, 0, "\"e\"", at(0)),
            RangeDecision::Unsatisfiable
        );
    }

    #[test]
    fn if_range_falls_back_to_full_on_validator_mismatch() {
        let ok = headers(&[(&header::RANGE, "bytes=0-9"), (&header::IF_RANGE, "\"e\"")]);
        assert!(matches!(
            decide_range(&ok, 100, "\"e\"", at(0)),
            RangeDecision::Partial(_)
        ));
        let stale = headers(&[
            (&header::RANGE, "bytes=0-9"),
            (&header::IF_RANGE, "\"old\""),
        ]);
        assert_eq!(
            decide_range(&stale, 100, "\"e\"", at(0)),
            RangeDecision::Full
        );
        let date_ok = headers(&[
            (&header::RANGE, "bytes=0-9"),
            (&header::IF_RANGE, &format_http_date(at(77))),
        ]);
        assert!(matches!(
            decide_range(&date_ok, 100, "\"e\"", at(77)),
            RangeDecision::Partial(_)
        ));
        assert_eq!(
            decide_range(&date_ok, 100, "\"e\"", at(78)),
            RangeDecision::Full
        );
        let weak = headers(&[
            (&header::RANGE, "bytes=0-9"),
            (&header::IF_RANGE, "W/\"e\""),
        ]);
        assert_eq!(
            decide_range(&weak, 100, "\"e\"", at(0)),
            RangeDecision::Full
        );
    }
}
