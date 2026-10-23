//! The metric series this crate emits through the `metrics` facade, with the names and labels
//! that `docs/slo.md` and `deploy/monitoring/rustryx-alerts.yaml` query.
//!
//! One module owns every name and label value so that a rename is a one line change and the
//! unit tests can pin the vocabulary the alerts depend on:
//!
//! | series | type | labels | used by |
//! |---|---|---|---|
//! | `auth_attempts_total` | counter | `method`, `result` | `RustryxAuthFailSpike` |
//! | `oidc_requests_total` | counter | `op` (`jwks`, `discovery`), `result` (`ok`, `error`, `timeout`) | `RustryxOidcRefreshFailures` |
//! | `oidc_jwks_age_seconds` | gauge | none | `RustryxOidcJwksStale` |
//! | `ldap_requests_total` | counter | `result` (`ok`, `error`, `timeout`) | `RustryxLdapErrors` |
//! | `auth_breakglass_logins_total` | counter | none | `RustryxBreakGlassLogin` |
//!
//! Label values are bounded constants. A user name, DN, group, issuer or error text never
//! becomes a label: the cardinality rules of docs/wave2-design.md section 2.6 forbid it, and
//! a name in a label is a data leak into the monitoring system.

use rustryx_core::error::AuthError;
use rustryx_core::identity::AuthMethod;

/// Series names. `pub` so the composition root and the tests can compare them with the
/// catalogue in `docs/slo.md`.
pub mod names {
    /// Counter, labels `method` and `result` (`ok`, `denied`, `error`).
    pub const AUTH_ATTEMPTS_TOTAL: &str = "auth_attempts_total";
    /// Counter, labels `op` (`jwks`, `discovery`) and `result` (`ok`, `error`, `timeout`).
    pub const OIDC_REQUESTS_TOTAL: &str = "oidc_requests_total";
    /// Gauge without labels: seconds since the last successful JWKS fetch, or since start
    /// while there has been none.
    pub const OIDC_JWKS_AGE_SECONDS: &str = "oidc_jwks_age_seconds";
    /// Counter, label `result` (`ok`, `error`, `timeout`).
    pub const LDAP_REQUESTS_TOTAL: &str = "ldap_requests_total";
    /// Counter without labels.
    pub const AUTH_BREAKGLASS_LOGINS_TOTAL: &str = "auth_breakglass_logins_total";
}

/// How an outgoing request to an identity provider ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestResult {
    /// The provider answered. For LDAP a refused password is an answer, not a failure.
    Ok,
    /// The request failed: transport, TLS, status or protocol.
    Error,
    /// The request ran into its timeout.
    Timeout,
}

impl RequestResult {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }
}

/// What a Keycloak fetch was for. Both values appear in the alert expression
/// `op=~"jwks|discovery"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OidcOp {
    Jwks,
    Discovery,
}

impl OidcOp {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Jwks => "jwks",
            Self::Discovery => "discovery",
        }
    }
}

/// The `method` label of `auth_attempts_total`. The vocabulary is the one the HTTP layer
/// already uses for the same series, so that moving the emission from one place to the
/// other keeps the dashboards intact. A method this build does not know is `unknown`.
pub(crate) const fn method_label(method: Option<AuthMethod>) -> &'static str {
    match method {
        Some(AuthMethod::Anonymous) => "anonymous",
        Some(AuthMethod::BasicLocal) => "local",
        Some(AuthMethod::BasicLdap) => "ldap",
        Some(AuthMethod::ApiToken) => "token",
        Some(AuthMethod::InternalJwt) => "internal_jwt",
        Some(AuthMethod::OidcBearer) => "oidc",
        Some(AuthMethod::RobotToken) => "robot",
        Some(AuthMethod::ScanToken) => "scan",
        // No method known (a refusal before any member looked at the credentials), or one
        // added to the non exhaustive `AuthMethod` later: counted as `unknown`, never dropped.
        None | Some(_) => "unknown",
    }
}

/// The `result` label of a failed attempt: `denied` is the caller's doing, `error` is ours or a
/// dependency's. `AccessDenied` is the caller's: the identity is real and maps to no access.
pub(crate) const fn failure_label(error: &AuthError) -> &'static str {
    match error {
        AuthError::InvalidCredentials
        | AuthError::InvalidToken(_)
        | AuthError::TokenExpired
        | AuthError::TokenRevoked
        | AuthError::AccountDisabled
        | AuthError::LockedOut { .. }
        | AuthError::UsernameConflict
        | AuthError::AccessDenied(_) => "denied",
        // The rest, and any variant added later, is a failure of ours or a dependency's: a
        // spike of `denied` pages somebody about credential stuffing, so an unknown variant
        // must not hide among them.
        _ => "error",
    }
}

/// Counts one authentication attempt.
pub(crate) fn auth_attempt(method: Option<AuthMethod>, failure: Option<&AuthError>) {
    metrics::counter!(
        names::AUTH_ATTEMPTS_TOTAL,
        "method" => method_label(method),
        "result" => failure.map_or("ok", failure_label),
    )
    .increment(1);
}

/// Counts one Keycloak fetch.
pub(crate) fn oidc_request(op: OidcOp, result: RequestResult) {
    metrics::counter!(
        names::OIDC_REQUESTS_TOTAL,
        "op" => op.label(),
        "result" => result.label(),
    )
    .increment(1);
}

/// Publishes the age of the cached JWKS.
pub(crate) fn oidc_jwks_age(seconds: f64) {
    metrics::gauge!(names::OIDC_JWKS_AGE_SECONDS).set(seconds);
}

/// Counts one operation sent to the directory.
pub(crate) fn ldap_request(result: RequestResult) {
    metrics::counter!(names::LDAP_REQUESTS_TOTAL, "result" => result.label()).increment(1);
}

/// Counts one login of the break-glass administrator.
pub(crate) fn breakglass_login() {
    metrics::counter!(names::AUTH_BREAKGLASS_LOGINS_TOTAL).increment(1);
}

/// A recorder for tests: keeps every counter and gauge by name and sorted labels, so a test
/// can assert the exact series an alert would see.
#[cfg(test)]
pub(crate) mod recording {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use metrics::{
        Counter, CounterFn, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, Recorder,
        SharedString, Unit,
    };

    type Series = BTreeMap<String, Arc<AtomicU64>>;

    /// Series key in the form `name{label="value",...}` with labels sorted.
    fn render(key: &Key) -> String {
        let mut labels: Vec<String> = key
            .labels()
            .map(|label| format!("{}=\"{}\"", label.key(), label.value()))
            .collect();
        labels.sort();
        if labels.is_empty() {
            key.name().to_owned()
        } else {
            format!("{}{{{}}}", key.name(), labels.join(","))
        }
    }

    /// Captures what the code under test emits. Install it with [`Capture::install`] and keep
    /// the guard alive for the length of the test (the recorder is thread local, which is
    /// what a `#[tokio::test]` on the current thread runtime needs).
    #[derive(Clone, Default)]
    pub(crate) struct Capture {
        counters: Arc<Mutex<Series>>,
        gauges: Arc<Mutex<Series>>,
    }

    struct Handle(Capture);

    impl Recorder for Handle {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            let mut series = self
                .0
                .counters
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let cell = series.entry(render(key)).or_default();
            Counter::from_arc(Arc::new(Shared(Arc::clone(cell))))
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            let mut series = self.0.gauges.lock().unwrap_or_else(PoisonError::into_inner);
            let cell = series.entry(render(key)).or_default();
            Gauge::from_arc(Arc::new(Shared(Arc::clone(cell))))
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

    /// A handle onto the shared cell of one series. A gauge keeps the bits of an `f64`.
    struct Shared(Arc<AtomicU64>);

    impl CounterFn for Shared {
        fn increment(&self, value: u64) {
            self.0.fetch_add(value, Ordering::SeqCst);
        }
        fn absolute(&self, value: u64) {
            self.0.fetch_max(value, Ordering::SeqCst);
        }
    }

    impl GaugeFn for Shared {
        fn increment(&self, value: f64) {
            let current = f64::from_bits(self.0.load(Ordering::SeqCst));
            self.0.store((current + value).to_bits(), Ordering::SeqCst);
        }
        fn decrement(&self, value: f64) {
            let current = f64::from_bits(self.0.load(Ordering::SeqCst));
            self.0.store((current - value).to_bits(), Ordering::SeqCst);
        }
        fn set(&self, value: f64) {
            self.0.store(value.to_bits(), Ordering::SeqCst);
        }
    }

    impl Capture {
        /// Makes this capture the recorder of the current thread until the guard drops.
        pub(crate) fn install(&self) -> metrics::LocalRecorderGuard<'static> {
            let handle: &'static Handle = Box::leak(Box::new(Handle(self.clone())));
            metrics::set_default_local_recorder(handle)
        }

        /// Value of a counter, `0` when it was never emitted.
        pub(crate) fn counter(&self, series: &str) -> u64 {
            self.counters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(series)
                .map_or(0, |cell| cell.load(Ordering::SeqCst))
        }

        /// Value of a gauge, `None` when it was never emitted.
        pub(crate) fn gauge(&self, series: &str) -> Option<f64> {
            self.gauges
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(series)
                .map(|cell| f64::from_bits(cell.load(Ordering::SeqCst)))
        }

        /// Every counter series name that was emitted.
        pub(crate) fn counter_names(&self) -> Vec<String> {
            self.counters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .cloned()
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::recording::Capture;
    use super::*;

    #[test]
    fn series_names_are_the_ones_the_alerts_query() {
        assert_eq!(names::AUTH_ATTEMPTS_TOTAL, "auth_attempts_total");
        assert_eq!(names::OIDC_REQUESTS_TOTAL, "oidc_requests_total");
        assert_eq!(names::OIDC_JWKS_AGE_SECONDS, "oidc_jwks_age_seconds");
        assert_eq!(names::LDAP_REQUESTS_TOTAL, "ldap_requests_total");
        assert_eq!(
            names::AUTH_BREAKGLASS_LOGINS_TOTAL,
            "auth_breakglass_logins_total"
        );
    }

    #[test]
    fn the_alert_expressions_match_the_emitted_label_values() {
        // `RustryxOidcRefreshFailures`: op=~"jwks|discovery", result!="ok".
        assert_eq!(OidcOp::Jwks.label(), "jwks");
        assert_eq!(OidcOp::Discovery.label(), "discovery");
        // `RustryxLdapErrors`: result=~"error|timeout".
        assert_eq!(RequestResult::Error.label(), "error");
        assert_eq!(RequestResult::Timeout.label(), "timeout");
        assert_eq!(RequestResult::Ok.label(), "ok");
    }

    #[test]
    fn method_labels_cover_every_method_with_the_http_layer_vocabulary() {
        for (method, label) in [
            (None, "unknown"),
            (Some(AuthMethod::Anonymous), "anonymous"),
            (Some(AuthMethod::BasicLocal), "local"),
            (Some(AuthMethod::BasicLdap), "ldap"),
            (Some(AuthMethod::ApiToken), "token"),
            (Some(AuthMethod::InternalJwt), "internal_jwt"),
            (Some(AuthMethod::OidcBearer), "oidc"),
            (Some(AuthMethod::RobotToken), "robot"),
            (Some(AuthMethod::ScanToken), "scan"),
        ] {
            assert_eq!(method_label(method), label);
        }
    }

    #[test]
    fn failures_are_denied_when_the_caller_is_at_fault_and_error_otherwise() {
        for denied in [
            AuthError::InvalidCredentials,
            AuthError::InvalidToken("x"),
            AuthError::TokenExpired,
            AuthError::TokenRevoked,
            AuthError::AccountDisabled,
            AuthError::LockedOut {
                retry_after: Duration::from_secs(1),
            },
            AuthError::UsernameConflict,
            AuthError::AccessDenied("x"),
        ] {
            assert_eq!(failure_label(&denied), "denied", "{denied:?}");
        }
        for failed in [
            AuthError::GroupLookupFailed("x".into()),
            AuthError::ProviderUnavailable("x".into()),
            AuthError::Overloaded,
            AuthError::GuardUnavailable,
        ] {
            assert_eq!(failure_label(&failed), "error", "{failed:?}");
        }
    }

    #[test]
    fn helpers_emit_exactly_the_documented_series() {
        let capture = Capture::default();
        let _guard = capture.install();
        auth_attempt(Some(AuthMethod::BasicLdap), None);
        auth_attempt(
            Some(AuthMethod::ApiToken),
            Some(&AuthError::InvalidCredentials),
        );
        auth_attempt(None, Some(&AuthError::Overloaded));
        oidc_request(OidcOp::Jwks, RequestResult::Timeout);
        ldap_request(RequestResult::Ok);
        breakglass_login();
        oidc_jwks_age(12.5);
        assert_eq!(
            capture.counter(r#"auth_attempts_total{method="ldap",result="ok"}"#),
            1
        );
        assert_eq!(
            capture.counter(r#"auth_attempts_total{method="token",result="denied"}"#),
            1
        );
        assert_eq!(
            capture.counter(r#"auth_attempts_total{method="unknown",result="error"}"#),
            1
        );
        assert_eq!(
            capture.counter(r#"oidc_requests_total{op="jwks",result="timeout"}"#),
            1
        );
        assert_eq!(capture.counter(r#"ldap_requests_total{result="ok"}"#), 1);
        assert_eq!(capture.counter("auth_breakglass_logins_total"), 1);
        assert_eq!(capture.gauge("oidc_jwks_age_seconds"), Some(12.5));
        assert_eq!(capture.counter_names().len(), 6);
    }
}
