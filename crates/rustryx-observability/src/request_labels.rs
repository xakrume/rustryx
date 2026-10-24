//! Labels of `http_request_duration_seconds` that only the layer that serves a request knows.
//!
//! The HTTP layer measures every request and records `route` and `method`. Two more labels
//! make the latency SLIs meaningful: `class` (what kind of request it was) and `cache`
//! (whether a proxy repository answered from its cache). The first is a property of the
//! protocol, the second of the proxy fill path, so only the format layer can say. It does
//! that in one of two ways, both read after the handler finished:
//!
//! - put a [`RequestLabels`] into the extensions of the **response** it returns, or
//! - when it has the request at hand, fetch the [`RequestLabelSlot`] that the HTTP layer put
//!   into the **request** extensions and call [`RequestLabelSlot::set_class`] or
//!   [`RequestLabelSlot::set_cache`].
//!
//! A response extension wins over the slot, and a label nobody set falls back to a default:
//! `class` from the route and method (`classify_default`), `cache` is `none`.

use std::sync::{Arc, Mutex, PoisonError};

/// What kind of request this was. A bounded vocabulary: it is a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RequestClass {
    /// Package metadata: indexes, manifests, packuments, `maven-metadata.xml`.
    Metadata,
    /// An artifact body served to a client.
    Download,
    /// An artifact body received from a client.
    Upload,
    /// The native API (`/api/...`).
    Api,
    /// Everything else (probes, scrapes, deletes, fallbacks).
    #[default]
    Other,
}

impl RequestClass {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Api => "api",
            Self::Other => "other",
        }
    }
}

/// Whether a proxy repository answered from its cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CacheStatus {
    /// Served from local storage without waiting for an upstream (a fresh or revalidated proxy
    /// copy, and a read of a hosted asset).
    Hit,
    /// Fetched from the upstream for this request.
    Miss,
    /// An expired copy served because the upstream is unavailable.
    Stale,
    /// Composed without consulting a cache (a merged group index).
    Bypass,
    /// Not a cacheable request (hosted repositories, API, probes).
    #[default]
    Uncached,
}

impl CacheStatus {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Stale => "stale",
            Self::Bypass => "bypass",
            Self::Uncached => "none",
        }
    }
}

/// Labels a handler wants on its request. `None` means "no opinion": the default applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestLabels {
    /// The request class, when the handler knows it better than the route does.
    pub class: Option<RequestClass>,
    /// The cache outcome.
    pub cache: Option<CacheStatus>,
}

impl RequestLabels {
    /// Labels with a class and no opinion about the cache.
    #[must_use]
    pub fn class(class: RequestClass) -> Self {
        Self {
            class: Some(class),
            cache: None,
        }
    }

    /// Adds the cache outcome.
    #[must_use]
    pub fn with_cache(mut self, cache: CacheStatus) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Fills what is missing from `fallback`.
    #[must_use]
    pub fn or(self, fallback: Self) -> Self {
        Self {
            class: self.class.or(fallback.class),
            cache: self.cache.or(fallback.cache),
        }
    }
}

/// A shared cell in the request extensions. Cloning shares the cell, so a handler that
/// received a clone through a task boundary still reports to the same request.
#[derive(Debug, Clone, Default)]
pub struct RequestLabelSlot(Arc<Mutex<RequestLabels>>);

impl RequestLabelSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the request class.
    pub fn set_class(&self, class: RequestClass) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).class = Some(class);
    }

    /// Sets the cache outcome.
    pub fn set_cache(&self, cache: CacheStatus) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).cache = Some(cache);
    }

    /// What was set so far.
    #[must_use]
    pub fn get(&self) -> RequestLabels {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The class of a request nobody labelled: the native API by route prefix, artifact
/// transfers by method, everything else `other`. `route` is the matched template.
#[must_use]
pub fn classify_default(route: &str, method: &str) -> RequestClass {
    if route == "/api" || route.starts_with("/api/") {
        return RequestClass::Api;
    }
    if matches!(route, "/healthz" | "/readyz" | "/metrics") {
        return RequestClass::Other;
    }
    let artifact_route = route.starts_with("/repository") || route.starts_with("/v2");
    match method {
        "GET" | "HEAD" if artifact_route => RequestClass::Download,
        "PUT" | "POST" | "PATCH" if artifact_route => RequestClass::Upload,
        _ => RequestClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_are_the_documented_vocabulary() {
        let classes: Vec<_> = [
            RequestClass::Metadata,
            RequestClass::Download,
            RequestClass::Upload,
            RequestClass::Api,
            RequestClass::Other,
        ]
        .iter()
        .map(|c| c.as_str())
        .collect();
        assert_eq!(classes, ["metadata", "download", "upload", "api", "other"]);
        assert_eq!(CacheStatus::Hit.as_str(), "hit");
        assert_eq!(CacheStatus::Miss.as_str(), "miss");
        assert_eq!(CacheStatus::Stale.as_str(), "stale");
        assert_eq!(CacheStatus::Bypass.as_str(), "bypass");
        assert_eq!(CacheStatus::Uncached.as_str(), "none");
        assert_eq!(RequestClass::default(), RequestClass::Other);
        assert_eq!(CacheStatus::default(), CacheStatus::Uncached);
    }

    #[test]
    fn explicit_labels_win_over_the_fallback_field_by_field() {
        let explicit = RequestLabels::class(RequestClass::Metadata);
        let fallback = RequestLabels::class(RequestClass::Download).with_cache(CacheStatus::Hit);
        let merged = explicit.or(fallback);
        assert_eq!(merged.class, Some(RequestClass::Metadata));
        assert_eq!(merged.cache, Some(CacheStatus::Hit));
        assert_eq!(
            RequestLabels::default().or(RequestLabels::default()),
            RequestLabels::default()
        );
    }

    #[test]
    fn a_slot_is_shared_between_clones() {
        let slot = RequestLabelSlot::new();
        let clone = slot.clone();
        assert_eq!(slot.get(), RequestLabels::default());
        clone.set_class(RequestClass::Metadata);
        clone.set_cache(CacheStatus::Miss);
        assert_eq!(
            slot.get(),
            RequestLabels::class(RequestClass::Metadata).with_cache(CacheStatus::Miss)
        );
    }

    #[test]
    fn the_default_class_follows_route_and_method() {
        let cases = [
            ("/api/v1/me", "GET", RequestClass::Api),
            ("/api", "POST", RequestClass::Api),
            ("/healthz", "GET", RequestClass::Other),
            ("/metrics", "GET", RequestClass::Other),
            ("/repository/{name}/{*path}", "GET", RequestClass::Download),
            ("/repository/{name}/{*path}", "HEAD", RequestClass::Download),
            ("/repository/{name}/{*path}", "PUT", RequestClass::Upload),
            ("/v2/{*rest}", "PATCH", RequestClass::Upload),
            ("/v2/{*rest}", "DELETE", RequestClass::Other),
            ("unmatched", "GET", RequestClass::Other),
            ("/apiary", "GET", RequestClass::Other),
        ];
        for (route, method, class) in cases {
            assert_eq!(classify_default(route, method), class, "{method} {route}");
        }
    }
}
