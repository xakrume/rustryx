//! Keyset pagination with opaque tokens.

use rustryx_core::error::StoreError;
use rustryx_core::ports::{Page, PageRequest};

/// Largest page any listing returns, whatever `PageRequest::limit` asks for.
pub const MAX_PAGE_LIMIT: u32 = 1000;

/// Marks tokens of this implementation, so a token from elsewhere is rejected early.
const TOKEN_PREFIX: &str = "m1.";

fn encode(key: &str) -> String {
    format!("{TOKEN_PREFIX}{}", hex::encode(key))
}

fn decode(token: &str) -> Result<String, StoreError> {
    let bad = || StoreError::Invalid("page token".to_owned());
    let hex_part = token.strip_prefix(TOKEN_PREFIX).ok_or_else(bad)?;
    let raw = hex::decode(hex_part).map_err(|_| bad())?;
    String::from_utf8(raw).map_err(|_| bad())
}

/// Returns the page of `rows` that follows the key encoded in `page.after`.
///
/// The token is the hex of the last key of the previous page, so it stays valid when
/// rows are inserted or deleted between requests (keyset, not offset). A token that cannot be
/// decoded is `StoreError::Invalid`, never `NotFound` or `Conflict` (gap 1 of core revision 2).
pub(crate) fn paginate<T>(
    mut rows: Vec<(String, T)>,
    page: &PageRequest,
) -> Result<Page<T>, StoreError> {
    let limit = usize::try_from(page.limit.clamp(1, MAX_PAGE_LIMIT)).unwrap_or(usize::MAX);
    let after = page.after.as_deref().map(decode).transpose()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut remaining: Vec<(String, T)> = rows
        .into_iter()
        .filter(|(key, _)| after.as_ref().is_none_or(|after| key > after))
        .collect();
    let more = remaining.len() > limit;
    remaining.truncate(limit);
    let next = if more {
        remaining.last().map(|(key, _)| encode(key))
    } else {
        None
    };
    Ok(Page {
        items: remaining.into_iter().map(|(_, item)| item).collect(),
        next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: u32) -> Vec<(String, u32)> {
        (0..n).map(|i| (format!("k{i:03}"), i)).collect()
    }

    fn request(limit: u32, after: Option<String>) -> PageRequest {
        PageRequest { limit, after }
    }

    #[test]
    fn walks_all_rows_page_by_page_without_repeats() {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = paginate(rows(25), &request(10, after)).unwrap();
            seen.extend(page.items);
            match page.next {
                Some(token) => after = Some(token),
                None => break,
            }
        }
        assert_eq!(seen, (0..25).collect::<Vec<_>>());
    }

    #[test]
    fn exact_multiple_of_the_limit_has_no_trailing_empty_page() {
        let page = paginate(rows(10), &request(10, None)).unwrap();
        assert_eq!(page.items.len(), 10);
        assert!(page.next.is_none());
    }

    #[test]
    fn limit_is_clamped_to_at_least_one_and_at_most_the_cap() {
        let one = paginate(rows(5), &request(0, None)).unwrap();
        assert_eq!(one.items.len(), 1);
        let many = paginate(rows(1500), &request(u32::MAX, None)).unwrap();
        assert_eq!(many.items.len(), 1000);
        assert!(many.next.is_some());
    }

    #[test]
    fn token_survives_deletion_of_the_row_it_points_at() {
        let first = paginate(rows(6), &request(3, None)).unwrap();
        let token = first.next.unwrap();
        let mut rest = rows(6);
        rest.retain(|(key, _)| key != "k002");
        let second = paginate(rest, &request(3, Some(token))).unwrap();
        assert_eq!(second.items, vec![3, 4, 5]);
    }

    #[test]
    fn rejects_tokens_it_did_not_issue() {
        for bad in ["", "garbage", "m1.zz", "m1.ff"] {
            let result = paginate(rows(3), &request(10, Some(bad.to_owned())));
            assert!(matches!(result, Err(StoreError::Invalid(_))), "{bad}");
        }
    }
}
