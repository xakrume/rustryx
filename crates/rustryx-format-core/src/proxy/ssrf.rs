//! SSRF guard for upstream URLs (SEC-34, SEC-27).
//!
//! Two checks, and the second one is the one that matters:
//!
//! 1. [`UpstreamUrlPolicy::check_url`] runs before a request is built: scheme
//!    allowlist, host allowlist, port allowlist, no userinfo, and every IP literal
//!    (the `url` crate has already folded `2130706433`, `0x7f.1` and `017700000001`
//!    into dotted form) goes through the address filter.
//! 2. [`GuardedResolver`] is installed as the HTTP client's DNS resolver. It
//!    resolves once, filters the answer, and hands exactly those addresses to the
//!    connector. There is no second lookup between "check" and "connect", so a DNS
//!    name with TTL 0 cannot answer a public address to the check and `127.0.0.1`
//!    to the connection (rebinding). TLS server name and `Host` stay the original
//!    host name, so certificate verification is unaffected by the pinning.
//!
//! An answer that contains even one forbidden address is rejected as a whole
//! instead of being filtered down: a mixed answer is what a rebinding attempt
//! looks like, and a legitimate public host has no reason to publish one.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// Class of an address that is not a public unicast address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressClass {
    /// `127.0.0.0/8`, `::1`.
    Loopback,
    /// RFC 1918, ULA `fc00::/7`, deprecated site-local `fec0::/10`.
    Private,
    /// `169.254.0.0/16`, `fe80::/10`.
    LinkLocal,
    /// Carrier-grade NAT `100.64.0.0/10`.
    CarrierGradeNat,
    /// `224.0.0.0/4`, `ff00::/8`. Never allowlistable.
    Multicast,
    /// `0.0.0.0`, `::`. Never allowlistable (connects to the local host on many stacks).
    Unspecified,
    /// A cloud metadata endpoint. Never allowlistable.
    Metadata,
    /// `255.255.255.255`. Never allowlistable.
    Broadcast,
    /// Other reserved space: `0.0.0.0/8`, `240.0.0.0/4`, IETF protocol ranges, Teredo, discard.
    Reserved,
    /// Documentation ranges (`192.0.2.0/24`, `2001:db8::/32`, ...).
    Documentation,
    /// Benchmarking range `198.18.0.0/15`.
    Benchmarking,
}

impl AddressClass {
    /// Whether an explicit CIDR allowlist entry may override the ban. Metadata,
    /// multicast, unspecified and broadcast addresses can never be reached.
    #[must_use]
    pub const fn is_overridable(self) -> bool {
        !matches!(
            self,
            Self::Multicast | Self::Unspecified | Self::Metadata | Self::Broadcast
        )
    }
}

/// Why a URL or a resolved address was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlRejection {
    /// Scheme other than https (or http for an allowlisted host).
    #[error("scheme {0:?} is not allowed")]
    Scheme(String),
    /// The URL has no host.
    #[error("url has no host")]
    NoHost,
    /// The URL carries user info; credentials come from the repository config only.
    #[error("credentials in url are not allowed")]
    Credentials,
    /// The host is not on the allowlist.
    #[error("host {0:?} is not on the allowlist")]
    HostNotAllowed(String),
    /// The port is not on the allowlist.
    #[error("port {0} is not allowed")]
    PortNotAllowed(u16),
    /// An address (literal or resolved) is in a forbidden class.
    #[error("address {ip} is forbidden ({class:?})")]
    ForbiddenAddress {
        /// The offending address.
        ip: IpAddr,
        /// Its class.
        class: AddressClass,
    },
    /// The name resolved to nothing.
    #[error("host did not resolve")]
    Unresolved,
}

/// A host name pattern: an exact name or `*.example.com` (any subdomain, not the apex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPattern {
    /// Matches this host only.
    Exact(String),
    /// Matches any host below this suffix.
    Subdomains(String),
}

impl HostPattern {
    /// Parses `example.com` or `*.example.com` (case-insensitive, trailing dot ignored).
    ///
    /// # Errors
    /// [`UrlRejection::HostNotAllowed`] for an empty pattern or a `*` anywhere else.
    pub fn parse(pattern: &str) -> Result<Self, UrlRejection> {
        let lowered = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
        if lowered.is_empty() {
            return Err(UrlRejection::HostNotAllowed(pattern.to_owned()));
        }
        if let Some(rest) = lowered.strip_prefix("*.") {
            if rest.is_empty() || rest.contains('*') {
                return Err(UrlRejection::HostNotAllowed(pattern.to_owned()));
            }
            return Ok(Self::Subdomains(rest.to_owned()));
        }
        if lowered.contains('*') {
            return Err(UrlRejection::HostNotAllowed(pattern.to_owned()));
        }
        Ok(Self::Exact(lowered))
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(name) => host == name,
            Self::Subdomains(suffix) => host
                .strip_suffix(suffix.as_str())
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1),
        }
    }
}

/// Which hosts a proxy repository may talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAllowlist {
    /// Any host, as long as every address it resolves to is public.
    AnyPublic,
    /// Only these hosts.
    Only(Vec<HostPattern>),
}

/// Policy for upstream URLs. The `Default` denies every host: allowing is an
/// explicit act of the composition root, built from configuration.
#[derive(Debug, Clone)]
pub struct UpstreamUrlPolicy {
    /// Host allowlist. Also applied to redirect targets.
    pub hosts: HostAllowlist,
    /// Hosts for which plain `http://` is accepted (SEC-27: only by explicit allowlist).
    pub http_hosts: Vec<HostPattern>,
    /// Explicit CIDRs of internal upstreams that may be reached although they are
    /// private, loopback, link-local, CGNAT or reserved. Metadata, multicast,
    /// unspecified and broadcast addresses cannot be allowlisted.
    pub allow_cidrs: Vec<IpNet>,
    /// Allowed destination ports. `None` means any.
    pub ports: Option<Vec<u16>>,
    /// Redirects followed per request, each one re-validated (SEC-34: at most 3).
    pub max_redirects: u8,
}

impl Default for UpstreamUrlPolicy {
    fn default() -> Self {
        Self {
            hosts: HostAllowlist::Only(Vec::new()),
            http_hosts: Vec::new(),
            allow_cidrs: Vec::new(),
            ports: None,
            max_redirects: 3,
        }
    }
}

impl UpstreamUrlPolicy {
    /// A policy that accepts any host over https as long as it resolves to public
    /// addresses. Convenient default for internet upstreams; internal ones need
    /// [`UpstreamUrlPolicy::allow_cidrs`].
    #[must_use]
    pub fn any_public() -> Self {
        Self {
            hosts: HostAllowlist::AnyPublic,
            ..Self::default()
        }
    }

    /// Validates a URL before any network activity.
    ///
    /// # Errors
    /// The first violated rule as a [`UrlRejection`].
    pub fn check_url(&self, url: &Url) -> Result<(), UrlRejection> {
        let host = url.host().ok_or(UrlRejection::NoHost)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UrlRejection::Credentials);
        }
        let host_text = match &host {
            Host::Domain(name) => name.trim_end_matches('.').to_ascii_lowercase(),
            Host::Ipv4(ip) => ip.to_string(),
            Host::Ipv6(ip) => ip.to_string(),
        };
        match url.scheme() {
            "https" => {}
            "http" if self.http_hosts.iter().any(|p| p.matches(&host_text)) => {}
            other => return Err(UrlRejection::Scheme(other.to_owned())),
        }
        if let HostAllowlist::Only(patterns) = &self.hosts
            && !patterns.iter().any(|p| p.matches(&host_text))
        {
            return Err(UrlRejection::HostNotAllowed(host_text));
        }
        if let (Some(ports), Some(port)) = (&self.ports, url.port_or_known_default())
            && !ports.contains(&port)
        {
            return Err(UrlRejection::PortNotAllowed(port));
        }
        match host {
            Host::Ipv4(ip) => self.check_ip(IpAddr::V4(ip)),
            Host::Ipv6(ip) => self.check_ip(IpAddr::V6(ip)),
            Host::Domain(_) => Ok(()),
        }
    }

    /// Validates one address (an IP literal in a URL or one DNS answer).
    ///
    /// # Errors
    /// [`UrlRejection::ForbiddenAddress`] when the address is not public and no
    /// allowlisted CIDR covers it.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), UrlRejection> {
        let canonical = canonical_ip(ip);
        let Some(class) = classify(canonical) else {
            return Ok(());
        };
        let allowed = class.is_overridable()
            && self
                .allow_cidrs
                .iter()
                .any(|net| net.contains(&canonical) || net.contains(&ip));
        if allowed {
            Ok(())
        } else {
            Err(UrlRejection::ForbiddenAddress { ip, class })
        }
    }

    /// Validates a whole DNS answer. One bad address rejects the answer.
    ///
    /// # Errors
    /// [`UrlRejection::Unresolved`] for an empty answer, otherwise the first forbidden address.
    pub fn validate_resolved(&self, ips: &[IpAddr]) -> Result<Vec<IpAddr>, UrlRejection> {
        if ips.is_empty() {
            return Err(UrlRejection::Unresolved);
        }
        for ip in ips {
            self.check_ip(*ip)?;
        }
        Ok(ips.to_vec())
    }
}

/// Unwraps IPv4 addresses that are embedded in IPv6 forms (mapped, compatible,
/// NAT64, 6to4) so that `::ffff:127.0.0.1` is judged as `127.0.0.1`.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => embedded_v4(v6).map_or(ip, IpAddr::V4),
    }
}

fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let seg = ip.segments();
    let o = ip.octets();
    let v4_from = |a: usize| Ipv4Addr::new(o[a], o[a + 1], o[a + 2], o[a + 3]);
    // ::ffff:a.b.c.d (mapped)
    if seg[..5] == [0, 0, 0, 0, 0] && seg[5] == 0xffff {
        return Some(v4_from(12));
    }
    // ::a.b.c.d (deprecated compatible), excluding :: and ::1
    if seg[..6] == [0, 0, 0, 0, 0, 0] && ip != Ipv6Addr::UNSPECIFIED && ip != Ipv6Addr::LOCALHOST {
        return Some(v4_from(12));
    }
    // 64:ff9b::/96 (NAT64)
    if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6] == [0, 0, 0, 0] {
        return Some(v4_from(12));
    }
    // 2002::/16 (6to4): the IPv4 address sits in bits 16..48
    if seg[0] == 0x2002 {
        return Some(v4_from(2));
    }
    None
}

fn in_v4(ip: Ipv4Addr, base: [u8; 4], prefix: u8) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(base)) & mask)
}

fn classify(ip: IpAddr) -> Option<AddressClass> {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> Option<AddressClass> {
    const METADATA: [[u8; 4]; 5] = [
        [169, 254, 169, 254], // AWS, GCP, Azure IMDS, OpenStack
        [169, 254, 170, 2],   // AWS ECS task metadata
        [168, 63, 129, 16],   // Azure wire server (a public address, so it needs naming)
        [100, 100, 100, 200], // Alibaba Cloud
        [192, 0, 0, 192],     // Oracle Cloud
    ];
    if ip == Ipv4Addr::UNSPECIFIED {
        return Some(AddressClass::Unspecified);
    }
    if ip == Ipv4Addr::BROADCAST {
        return Some(AddressClass::Broadcast);
    }
    if METADATA.iter().any(|m| ip.octets() == *m) {
        return Some(AddressClass::Metadata);
    }
    let table: [([u8; 4], u8, AddressClass); 14] = [
        ([0, 0, 0, 0], 8, AddressClass::Reserved),
        ([127, 0, 0, 0], 8, AddressClass::Loopback),
        ([10, 0, 0, 0], 8, AddressClass::Private),
        ([172, 16, 0, 0], 12, AddressClass::Private),
        ([192, 168, 0, 0], 16, AddressClass::Private),
        ([169, 254, 0, 0], 16, AddressClass::LinkLocal),
        ([100, 64, 0, 0], 10, AddressClass::CarrierGradeNat),
        ([224, 0, 0, 0], 4, AddressClass::Multicast),
        ([240, 0, 0, 0], 4, AddressClass::Reserved),
        ([192, 0, 0, 0], 24, AddressClass::Reserved),
        ([192, 88, 99, 0], 24, AddressClass::Reserved),
        ([192, 0, 2, 0], 24, AddressClass::Documentation),
        ([198, 51, 100, 0], 24, AddressClass::Documentation),
        ([198, 18, 0, 0], 15, AddressClass::Benchmarking),
    ];
    if let Some((_, _, class)) = table
        .iter()
        .find(|(base, prefix, _)| in_v4(ip, *base, *prefix))
    {
        return Some(*class);
    }
    if in_v4(ip, [203, 0, 113, 0], 24) {
        return Some(AddressClass::Documentation);
    }
    None
}

fn classify_v6(ip: Ipv6Addr) -> Option<AddressClass> {
    let seg = ip.segments();
    if ip == Ipv6Addr::UNSPECIFIED {
        return Some(AddressClass::Unspecified);
    }
    if ip == Ipv6Addr::LOCALHOST {
        return Some(AddressClass::Loopback);
    }
    // fd00:ec2::254, the AWS IMDS over IPv6.
    if seg == [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254] {
        return Some(AddressClass::Metadata);
    }
    if seg[0] & 0xff00 == 0xff00 {
        return Some(AddressClass::Multicast);
    }
    if seg[0] & 0xfe00 == 0xfc00 || seg[0] & 0xffc0 == 0xfec0 {
        return Some(AddressClass::Private);
    }
    if seg[0] & 0xffc0 == 0xfe80 {
        return Some(AddressClass::LinkLocal);
    }
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return Some(AddressClass::Documentation);
    }
    // Teredo 2001::/32, discard-only 100::/64, local-use NAT64 64:ff9b:1::/48.
    if (seg[0] == 0x2001 && seg[1] == 0)
        || (seg[0] == 0x0100 && seg[1..4] == [0, 0, 0])
        || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2] == 1)
    {
        return Some(AddressClass::Reserved);
    }
    None
}

/// Name resolution behind the guard. A trait so tests can script DNS answers.
#[async_trait]
pub trait HostResolver: Send + Sync + 'static {
    /// Resolves a host name to its addresses. Cancel safe.
    async fn lookup(&self, host: &str) -> io::Result<Vec<IpAddr>>;
}

/// The operating system resolver (`getaddrinfo` through tokio).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

#[async_trait]
impl HostResolver for SystemResolver {
    async fn lookup(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((host, 0)).await?;
        Ok(addrs.map(|addr| addr.ip()).collect())
    }
}

/// DNS resolver for the HTTP client that refuses forbidden answers.
///
/// See the module docs: this is where the check and the connection are made one
/// step, which is what defeats DNS rebinding.
pub struct GuardedResolver {
    policy: Arc<UpstreamUrlPolicy>,
    inner: Arc<dyn HostResolver>,
}

impl GuardedResolver {
    /// Wraps `inner` with `policy`.
    #[must_use]
    pub fn new(policy: Arc<UpstreamUrlPolicy>, inner: Arc<dyn HostResolver>) -> Self {
        Self { policy, inner }
    }

    /// Resolves and validates. Public for tests and for callers that need the same guard.
    ///
    /// # Errors
    /// An I/O error from the resolver, or a [`UrlRejection`] wrapped in [`io::Error`].
    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<IpAddr>, ResolveError> {
        let ips = self
            .inner
            .lookup(host)
            .await
            .map_err(ResolveError::Lookup)?;
        self.policy
            .validate_resolved(&ips)
            .map_err(ResolveError::Rejected)
    }
}

/// Failure of [`GuardedResolver::resolve_checked`].
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The system resolver failed.
    #[error("dns lookup failed: {0}")]
    Lookup(io::Error),
    /// The answer violated the policy. The HTTP client surfaces this error in its
    /// source chain, which is how a refusal is told apart from a network failure.
    #[error(transparent)]
    Rejected(UrlRejection),
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let guard = GuardedResolver { policy, inner };
            let ips = guard.resolve_checked(name.as_str()).await?;
            let addrs: Addrs = Box::new(
                ips.into_iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> UpstreamUrlPolicy {
        UpstreamUrlPolicy::any_public()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn rejected_class(ip: &str) -> Option<AddressClass> {
        match policy().check_ip(ip.parse().unwrap()) {
            Err(UrlRejection::ForbiddenAddress { class, .. }) => Some(class),
            _ => None,
        }
    }

    #[test]
    fn must_not_reach_loopback() {
        assert_eq!(rejected_class("127.0.0.1"), Some(AddressClass::Loopback));
        assert_eq!(
            rejected_class("127.255.255.254"),
            Some(AddressClass::Loopback)
        );
        assert_eq!(rejected_class("::1"), Some(AddressClass::Loopback));
    }

    #[test]
    fn must_not_reach_private_ranges() {
        for ip in [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
        ] {
            assert_eq!(rejected_class(ip), Some(AddressClass::Private), "{ip}");
        }
        for ip in ["172.15.255.255", "172.32.0.1", "11.0.0.1", "192.169.0.1"] {
            assert_eq!(rejected_class(ip), None, "{ip} is public");
        }
    }

    #[test]
    fn must_not_reach_link_local_and_metadata() {
        assert_eq!(rejected_class("169.254.1.1"), Some(AddressClass::LinkLocal));
        assert_eq!(rejected_class("fe80::1"), Some(AddressClass::LinkLocal));
        for ip in [
            "169.254.169.254",
            "169.254.170.2",
            "168.63.129.16",
            "100.100.100.200",
            "192.0.0.192",
            "fd00:ec2::254",
        ] {
            assert_eq!(rejected_class(ip), Some(AddressClass::Metadata), "{ip}");
        }
    }

    #[test]
    fn must_not_reach_cgnat_multicast_unspecified_broadcast() {
        assert_eq!(
            rejected_class("100.64.0.1"),
            Some(AddressClass::CarrierGradeNat)
        );
        assert_eq!(
            rejected_class("100.127.255.255"),
            Some(AddressClass::CarrierGradeNat)
        );
        assert_eq!(rejected_class("100.128.0.1"), None);
        assert_eq!(rejected_class("224.0.0.1"), Some(AddressClass::Multicast));
        assert_eq!(
            rejected_class("239.255.255.255"),
            Some(AddressClass::Multicast)
        );
        assert_eq!(rejected_class("ff02::1"), Some(AddressClass::Multicast));
        assert_eq!(rejected_class("0.0.0.0"), Some(AddressClass::Unspecified));
        assert_eq!(rejected_class("::"), Some(AddressClass::Unspecified));
        assert_eq!(
            rejected_class("255.255.255.255"),
            Some(AddressClass::Broadcast)
        );
    }

    #[test]
    fn must_not_reach_reserved_documentation_and_benchmark_space() {
        for ip in ["0.1.2.3", "240.0.0.1", "192.0.0.1", "192.88.99.1"] {
            assert_eq!(rejected_class(ip), Some(AddressClass::Reserved), "{ip}");
        }
        for ip in ["192.0.2.1", "198.51.100.1", "203.0.113.1", "2001:db8::1"] {
            assert_eq!(
                rejected_class(ip),
                Some(AddressClass::Documentation),
                "{ip}"
            );
        }
        assert_eq!(
            rejected_class("198.18.0.1"),
            Some(AddressClass::Benchmarking)
        );
        assert_eq!(rejected_class("2001::1"), Some(AddressClass::Reserved));
        assert_eq!(rejected_class("100::1"), Some(AddressClass::Reserved));
        assert_eq!(rejected_class("64:ff9b:1::1"), Some(AddressClass::Reserved));
    }

    #[test]
    fn must_not_reach_internal_addresses_hidden_in_ipv6_forms() {
        for (ip, class) in [
            ("::ffff:127.0.0.1", AddressClass::Loopback),
            ("::ffff:10.1.2.3", AddressClass::Private),
            ("::ffff:169.254.169.254", AddressClass::Metadata),
            ("::127.0.0.1", AddressClass::Loopback),
            ("64:ff9b::7f00:1", AddressClass::Loopback),
            ("2002:7f00:1::1", AddressClass::Loopback),
            ("2002:c0a8:101::1", AddressClass::Private),
        ] {
            assert_eq!(rejected_class(ip), Some(class), "{ip}");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(policy().check_ip(ip.parse().unwrap()).is_ok(), "{ip}");
        }
    }

    #[test]
    fn must_not_accept_alternate_ip_notations_for_loopback() {
        for text in [
            "https://2130706433/",
            "https://0x7f000001/",
            "https://0x7f.0.0.1/",
            "https://017700000001/",
            "https://127.1/",
            "https://[::1]/",
            "https://[::ffff:7f00:1]/",
            "https://0/",
            "https://[::]/",
        ] {
            assert!(policy().check_url(&url(text)).is_err(), "{text}");
        }
    }

    #[test]
    fn must_not_reach_metadata_by_url() {
        assert!(matches!(
            policy().check_url(&url("https://169.254.169.254/latest/meta-data/")),
            Err(UrlRejection::ForbiddenAddress {
                class: AddressClass::Metadata,
                ..
            })
        ));
    }

    #[test]
    fn scheme_allowlist() {
        let p = policy();
        for bad in [
            "http://example.com/",
            "ftp://example.com/",
            "file:///etc/passwd",
            "gopher://example.com/",
        ] {
            let rejected = url::Url::parse(bad).map_or(true, |u| p.check_url(&u).is_err());
            assert!(rejected, "{bad}");
        }
        assert!(p.check_url(&url("https://example.com/x")).is_ok());
    }

    #[test]
    fn plain_http_needs_an_explicit_host_allowlist() {
        let mut p = policy();
        assert!(matches!(
            p.check_url(&url("http://mirror.example.com/")),
            Err(UrlRejection::Scheme(_))
        ));
        p.http_hosts = vec![HostPattern::parse("mirror.example.com").unwrap()];
        assert!(p.check_url(&url("http://mirror.example.com/")).is_ok());
        assert!(p.check_url(&url("http://other.example.com/")).is_err());
    }

    #[test]
    fn default_policy_denies_every_host() {
        assert!(matches!(
            UpstreamUrlPolicy::default().check_url(&url("https://example.com/")),
            Err(UrlRejection::HostNotAllowed(_))
        ));
    }

    #[test]
    fn host_allowlist_exact_and_wildcard() {
        let p = UpstreamUrlPolicy {
            hosts: HostAllowlist::Only(vec![
                HostPattern::parse("repo1.maven.org").unwrap(),
                HostPattern::parse("*.cdn.example.com").unwrap(),
            ]),
            ..UpstreamUrlPolicy::default()
        };
        assert!(p.check_url(&url("https://repo1.maven.org/maven2/")).is_ok());
        assert!(
            p.check_url(&url("https://REPO1.MAVEN.ORG./maven2/"))
                .is_ok()
        );
        assert!(p.check_url(&url("https://a.cdn.example.com/x")).is_ok());
        assert!(p.check_url(&url("https://a.b.cdn.example.com/x")).is_ok());
        for bad in [
            "https://cdn.example.com/x",
            "https://evilcdn.example.com/x",
            "https://repo1.maven.org.evil.com/x",
            "https://evil.com/repo1.maven.org",
            "https://xrepo1.maven.org/",
        ] {
            assert!(
                matches!(p.check_url(&url(bad)), Err(UrlRejection::HostNotAllowed(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn host_pattern_parsing() {
        assert_eq!(
            HostPattern::parse("Example.COM.").unwrap(),
            HostPattern::Exact("example.com".into())
        );
        assert_eq!(
            HostPattern::parse("*.Example.com").unwrap(),
            HostPattern::Subdomains("example.com".into())
        );
        for bad in ["", "  ", "*", "*.", "a.*.com", "*.*.com", "ex*.com"] {
            assert!(HostPattern::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn must_not_accept_userinfo_in_url() {
        assert_eq!(
            policy().check_url(&url("https://user:pass@example.com/")),
            Err(UrlRejection::Credentials)
        );
        assert_eq!(
            policy().check_url(&url("https://user@example.com/")),
            Err(UrlRejection::Credentials)
        );
        // The classic parser trick: the host is evil.com, not example.com.
        let tricky = url("https://example.com@evil.com/");
        assert_eq!(policy().check_url(&tricky), Err(UrlRejection::Credentials));
    }

    #[test]
    fn port_allowlist() {
        let p = UpstreamUrlPolicy {
            ports: Some(vec![443, 8443]),
            ..UpstreamUrlPolicy::any_public()
        };
        assert!(p.check_url(&url("https://example.com/")).is_ok());
        assert!(p.check_url(&url("https://example.com:8443/")).is_ok());
        assert_eq!(
            p.check_url(&url("https://example.com:22/")),
            Err(UrlRejection::PortNotAllowed(22))
        );
    }

    #[test]
    fn allow_cidrs_override_private_but_never_metadata() {
        let p = UpstreamUrlPolicy {
            allow_cidrs: vec![
                "10.20.0.0/16".parse().unwrap(),
                "169.254.0.0/16".parse().unwrap(),
                "0.0.0.0/0".parse().unwrap(),
            ],
            ..UpstreamUrlPolicy::any_public()
        };
        assert!(p.check_ip("10.20.1.1".parse().unwrap()).is_ok());
        assert!(p.check_ip("169.254.1.1".parse().unwrap()).is_ok());
        assert!(p.check_ip("::ffff:10.20.1.1".parse().unwrap()).is_ok());
        for forbidden in ["169.254.169.254", "0.0.0.0", "224.0.0.1", "255.255.255.255"] {
            assert!(
                p.check_ip(forbidden.parse().unwrap()).is_err(),
                "{forbidden} must stay forbidden even with a catch-all CIDR"
            );
        }
        let narrow = UpstreamUrlPolicy {
            allow_cidrs: vec!["10.20.0.0/16".parse().unwrap()],
            ..UpstreamUrlPolicy::any_public()
        };
        assert!(narrow.check_ip("10.21.0.1".parse().unwrap()).is_err());
        assert!(narrow.check_ip("127.0.0.1".parse().unwrap()).is_err());
    }

    #[test]
    fn overridable_classes() {
        assert!(AddressClass::Loopback.is_overridable());
        assert!(AddressClass::Private.is_overridable());
        assert!(!AddressClass::Metadata.is_overridable());
        assert!(!AddressClass::Multicast.is_overridable());
        assert!(!AddressClass::Unspecified.is_overridable());
        assert!(!AddressClass::Broadcast.is_overridable());
    }

    #[test]
    fn must_reject_dns_answer_with_any_forbidden_address() {
        let p = policy();
        let mixed: Vec<IpAddr> = vec![
            "93.184.216.34".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ];
        assert!(p.validate_resolved(&mixed).is_err());
        assert_eq!(p.validate_resolved(&[]), Err(UrlRejection::Unresolved));
        let ok: Vec<IpAddr> = vec![
            "93.184.216.34".parse().unwrap(),
            "2606:4700::1".parse().unwrap(),
        ];
        assert_eq!(p.validate_resolved(&ok).unwrap(), ok);
    }

    struct Scripted(Vec<IpAddr>);

    #[async_trait]
    impl HostResolver for Scripted {
        async fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    struct Failing;

    #[async_trait]
    impl HostResolver for Failing {
        async fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
            Err(io::Error::other("nxdomain"))
        }
    }

    #[tokio::test]
    async fn guarded_resolver_returns_validated_addresses_only() {
        let resolver = GuardedResolver::new(
            Arc::new(policy()),
            Arc::new(Scripted(vec!["93.184.216.34".parse().unwrap()])),
        );
        let ips = resolver.resolve_checked("example.com").await.unwrap();
        assert_eq!(ips, vec!["93.184.216.34".parse::<IpAddr>().unwrap()]);

        let rebinding = GuardedResolver::new(
            Arc::new(policy()),
            Arc::new(Scripted(vec!["127.0.0.1".parse().unwrap()])),
        );
        assert!(matches!(
            rebinding.resolve_checked("evil.example").await,
            Err(ResolveError::Rejected(
                UrlRejection::ForbiddenAddress { .. }
            ))
        ));

        let broken = GuardedResolver::new(Arc::new(policy()), Arc::new(Failing));
        assert!(matches!(
            broken.resolve_checked("x").await,
            Err(ResolveError::Lookup(_))
        ));
    }

    #[tokio::test]
    async fn guarded_resolver_implements_the_http_client_trait() {
        let resolver = GuardedResolver::new(
            Arc::new(policy()),
            Arc::new(Scripted(vec!["93.184.216.34".parse().unwrap()])),
        );
        let name: Name = "example.com".parse().unwrap();
        let addrs: Vec<SocketAddr> = resolver.resolve(name).await.unwrap().collect();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), "93.184.216.34".parse::<IpAddr>().unwrap());

        let bad = GuardedResolver::new(
            Arc::new(policy()),
            Arc::new(Scripted(vec!["10.0.0.1".parse().unwrap()])),
        );
        let name: Name = "internal.example".parse().unwrap();
        assert!(bad.resolve(name).await.is_err());
    }

    #[tokio::test]
    async fn system_resolver_resolves_localhost_and_the_guard_refuses_it() {
        let ips = SystemResolver.lookup("localhost").await.unwrap();
        assert!(ips.iter().any(IpAddr::is_loopback));
        let guarded = GuardedResolver::new(Arc::new(policy()), Arc::new(SystemResolver));
        assert!(matches!(
            guarded.resolve_checked("localhost").await,
            Err(ResolveError::Rejected(_))
        ));
    }
}
