//! Upstream validators of cached copies, for conditional revalidation.
//!
//! When a cached copy expires, asking the upstream "has it changed?" (`If-None-Match`,
//! `If-Modified-Since`) costs one small round trip, while fetching it again costs the whole
//! body. The validators to ask with are the ones the upstream attached to the copy we hold:
//! its own `ETag` and `Last-Modified`, sent back verbatim.
//!
//! Why they live in a per node cache and not in the database: `Asset` has no column for
//! them (reported as a core gap). The honest consequences, which `docs/format-core.md`
//! repeats:
//!
//! * the cache is keyed by `(asset, blob)`, the exact pair that identifies the cached bytes.
//!   An entry for a replaced blob is never found again, so a validator can never be sent for
//!   content it does not describe;
//! * a node that never filled the copy (cold start, another node filled it) has no validator,
//!   asks without a condition, and compares the digest of the answer with the copy it holds:
//!   an identical body is dropped instead of replacing the blob (see the fill);
//! * nothing is ever derived from our own timestamps. `If-Modified-Since` with the time WE
//!   cached the copy would miss a change made between the upstream's answer and our commit,
//!   and an upstream with one second timestamps would miss changes within the same second.

use std::time::Duration;

use http::{HeaderMap, HeaderValue, header};
use moka::future::Cache;
use rustryx_core::ids::{AssetId, BlobId};

/// The validators an upstream attached to one response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Validators {
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
}

impl Validators {
    /// Reads `ETag` and `Last-Modified` from an upstream response; `None` when it has neither.
    /// Both are kept verbatim: a weak `ETag` is fine for `If-None-Match` (weak comparison), and
    /// the date is the upstream's own, so no clock of ours is involved.
    pub(crate) fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let usable = |name: &header::HeaderName| {
            headers
                .get(name)
                .filter(|value| value.to_str().is_ok_and(|text| !text.trim().is_empty()))
                .cloned()
        };
        let validators = Self {
            etag: usable(&header::ETAG),
            last_modified: usable(&header::LAST_MODIFIED),
        };
        (validators.etag.is_some() || validators.last_modified.is_some()).then_some(validators)
    }

    /// Adds the conditional request headers. With both validators both are sent: an upstream
    /// that evaluates `If-None-Match` ignores the date, as RFC 9110 section 13.1.3 says.
    pub(crate) fn apply(&self, headers: &mut HeaderMap) {
        if let Some(etag) = &self.etag {
            headers.insert(header::IF_NONE_MATCH, etag.clone());
        }
        if let Some(date) = &self.last_modified {
            headers.insert(header::IF_MODIFIED_SINCE, date.clone());
        }
    }
}

/// Bounded cache of [`Validators`] by `(asset, blob)`.
#[derive(Clone)]
pub(crate) struct ValidatorCache {
    cache: Cache<(AssetId, BlobId), Validators>,
}

impl ValidatorCache {
    /// A cache of at most `capacity` entries that forgets an entry nobody asked for within
    /// `idle` (an expired copy past the stale limit is never revalidated again).
    pub(crate) fn new(capacity: u64, idle: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(idle)
                .build(),
        }
    }

    pub(crate) async fn get(&self, asset: AssetId, blob: BlobId) -> Option<Validators> {
        self.cache.get(&(asset, blob)).await
    }

    pub(crate) async fn put(&self, asset: AssetId, blob: BlobId, validators: Validators) {
        self.cache.insert((asset, blob), validators).await;
    }

    #[cfg(test)]
    pub(crate) async fn run_pending_tasks(&self) {
        self.cache.run_pending_tasks().await;
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn reads_both_validators_verbatim() {
        let found = Validators::from_headers(&headers(&[
            ("etag", "W/\"abc\""),
            ("last-modified", "Wed, 21 Oct 2026 07:28:00 GMT"),
        ]))
        .unwrap();
        let mut request = HeaderMap::new();
        found.apply(&mut request);
        assert_eq!(request.get(header::IF_NONE_MATCH).unwrap(), "W/\"abc\"");
        assert_eq!(
            request.get(header::IF_MODIFIED_SINCE).unwrap(),
            "Wed, 21 Oct 2026 07:28:00 GMT"
        );
    }

    #[test]
    fn one_validator_is_enough_and_none_is_none() {
        let etag_only = Validators::from_headers(&headers(&[("etag", "\"x\"")])).unwrap();
        let mut request = HeaderMap::new();
        etag_only.apply(&mut request);
        assert!(request.get(header::IF_MODIFIED_SINCE).is_none());
        assert!(Validators::from_headers(&headers(&[("content-type", "text/plain")])).is_none());
        assert!(
            Validators::from_headers(&headers(&[("etag", "  ")])).is_none(),
            "a blank validator proves nothing"
        );
    }

    #[tokio::test]
    async fn entries_are_found_only_for_the_exact_blob() {
        let cache = ValidatorCache::new(10, Duration::from_secs(60));
        let (asset, blob) = (AssetId::generate(), BlobId::generate());
        let validators = Validators::from_headers(&headers(&[("etag", "\"x\"")])).unwrap();
        cache.put(asset, blob, validators.clone()).await;
        assert_eq!(cache.get(asset, blob).await, Some(validators));
        assert!(
            cache.get(asset, BlobId::generate()).await.is_none(),
            "a replaced blob must not inherit the validators of the old bytes"
        );
        assert!(cache.get(AssetId::generate(), blob).await.is_none());
    }

    #[tokio::test]
    async fn must_stay_bounded() {
        let cache = ValidatorCache::new(10, Duration::from_secs(60));
        let validators = Validators::from_headers(&headers(&[("etag", "\"x\"")])).unwrap();
        for _ in 0..500 {
            cache
                .put(AssetId::generate(), BlobId::generate(), validators.clone())
                .await;
        }
        cache.run_pending_tasks().await;
        assert!(cache.entry_count() <= 10, "count {}", cache.entry_count());
    }
}
