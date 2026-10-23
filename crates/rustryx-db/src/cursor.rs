//! Opaque keyset pagination tokens.
//!
//! A token is `base64url("<kind>:<last key>")`. It carries a position, not a permission:
//! filters are applied in the `WHERE` clause independently of the token, so a forged or
//! replayed token can only move the window, never widen what the caller may see. A
//! signature is therefore unnecessary. The `kind` prefix makes a token of one listing
//! unusable in another (an asset path where a user id is expected).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustryx_core::StoreError;
use rustryx_core::ports::{Page, PageRequest};

/// Hard cap of a page, whatever the caller asks for.
pub const MAX_PAGE_LIMIT: u32 = 1000;

pub(crate) fn encode(kind: &str, key: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{kind}:{key}"))
}

/// The error of a token that cannot be used: [`StoreError::Invalid`], which the HTTP layer
/// maps to 400 `invalid_page_token`. It is not a `Conflict` (the request is malformed, not in
/// conflict with state) and it does not restart from the first page: a client that keeps
/// sending a bad token would then loop forever.
pub(crate) fn invalid_token() -> StoreError {
    StoreError::Invalid("invalid page token".to_owned())
}

/// Decodes the token of `kind`.
pub(crate) fn decode(kind: &str, token: &str) -> Result<String, StoreError> {
    let bad = invalid_token;
    let raw = URL_SAFE_NO_PAD.decode(token).map_err(|_| bad())?;
    let text = String::from_utf8(raw).map_err(|_| bad())?;
    let (found_kind, key) = text.split_once(':').ok_or_else(bad)?;
    if found_kind != kind {
        return Err(bad());
    }
    Ok(key.to_owned())
}

/// Number of rows to fetch for a page: one more than the caller asked for, to learn
/// whether a next page exists.
pub(crate) fn fetch_limit(request: &PageRequest) -> (usize, i64) {
    let limit = request.limit.clamp(1, MAX_PAGE_LIMIT);
    let limit_usize = usize::try_from(limit).unwrap_or(1);
    (limit_usize, i64::from(limit) + 1)
}

/// Cuts the fetched rows to the page size and builds the next token from the last kept row.
pub(crate) fn finish<T>(
    mut rows: Vec<T>,
    limit: usize,
    kind: &str,
    key_of: impl Fn(&T) -> String,
) -> Page<T> {
    let next = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|last| encode(kind, &key_of(last)))
    } else {
        None
    };
    Page { items: rows, next }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_binds_to_kind() {
        let token = encode("asset", "a/b:c");
        assert_eq!(decode("asset", &token).unwrap(), "a/b:c");
        assert!(decode("user", &token).is_err());
    }

    #[test]
    fn rejects_garbage_tokens_as_invalid_input() {
        for bad in ["!!!", "", "bm9jb2xvbg", "////"] {
            assert!(
                matches!(decode("asset", bad), Err(StoreError::Invalid(_))),
                "{bad}"
            );
        }
        let other_kind = encode("user", "x");
        assert!(matches!(
            decode("asset", &other_kind),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn limit_is_clamped() {
        let (n, fetch) = fetch_limit(&PageRequest {
            limit: 0,
            after: None,
        });
        assert_eq!((n, fetch), (1, 2));
        let (n, fetch) = fetch_limit(&PageRequest {
            limit: 10_000_000,
            after: None,
        });
        assert_eq!((n, fetch), (1000, 1001));
    }

    #[test]
    fn finish_sets_next_only_when_more_rows_exist() {
        let page = finish(vec![1, 2, 3], 2, "n", ToString::to_string);
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(decode("n", &page.next.unwrap()).unwrap(), "2");
        let last = finish(vec![1, 2], 2, "n", ToString::to_string);
        assert!(last.next.is_none());
    }
}
