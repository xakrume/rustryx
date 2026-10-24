//! The upstream URL policy of the harness.

use std::time::Duration;

use rustryx_format_core::proxy::breaker::BreakerConfig;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::{HostAllowlist, HostPattern, UpstreamUrlPolicy};

/// The policy a [`TestServer`](crate::TestServer) uses unless the test supplies its own.
///
/// It accepts any public host over https (so a test that points a proxy at a fake name still
/// reaches the `FakeUpstream`) and, **only for the loopback names**, plain http and the
/// loopback ranges, so that a local wiremock upstream works.
///
/// The production policy is stricter: `UpstreamUrlPolicy::default()` denies every host, and
/// the composition root allows named hosts, https only, with CIDRs for internal mirrors. A
/// test of the SSRF guard itself must pass its own policy through
/// `TestServerBuilder::upstream_policy`; this one is deliberately open to loopback.
#[must_use]
pub fn loopback_friendly_policy() -> UpstreamUrlPolicy {
    // `localhost` resolves to both families, and the guard rejects a DNS answer when any of its
    // addresses is not allowed (a mixed answer is what rebinding looks like), so both
    // loopback families must be listed.
    let http_hosts = ["127.0.0.1", "localhost", "::1"]
        .iter()
        .filter_map(|name| HostPattern::parse(name).ok())
        .collect();
    UpstreamUrlPolicy {
        hosts: HostAllowlist::AnyPublic,
        http_hosts,
        allow_cidrs: ["127.0.0.0/8", "::1/128"]
            .iter()
            .filter_map(|net| net.parse().ok())
            .collect(),
        ports: None,
        max_redirects: 3,
    }
}

/// Upstream timeouts and retries short enough for a failing-upstream test: the defaults back
/// off for seconds. Retries are kept (three attempts, one millisecond apart) so a test can
/// still see that a flaky upstream is retried; the breaker needs 50 outcomes to open, so a few
/// failing requests do not trip it.
#[must_use]
pub fn fast_executor_config() -> ExecutorConfig {
    ExecutorConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            jitter: false,
        },
        permit_wait: Duration::from_millis(300),
        idle_timeout: Duration::from_millis(500),
        metadata_timeout: Duration::from_millis(1000),
        breaker: BreakerConfig {
            min_requests: 50,
            ..BreakerConfig::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    #[test]
    fn loopback_over_http_is_allowed_and_other_private_ranges_are_not() {
        let policy = loopback_friendly_policy();
        let loopback = Url::parse("http://127.0.0.1:8080/x").unwrap();
        assert!(policy.check_url(&loopback).is_ok());
        let private = Url::parse("http://10.1.2.3/x").unwrap();
        assert!(
            policy.check_url(&private).is_err(),
            "http is only for loopback names"
        );
        let private_https = Url::parse("https://10.1.2.3/x").unwrap();
        assert!(policy.check_url(&private_https).is_err());
        let metadata = Url::parse("http://169.254.169.254/x").unwrap();
        assert!(policy.check_url(&metadata).is_err());
    }
}
