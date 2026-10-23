//! Client address behind reverse proxies.
//!
//! `X-Forwarded-For` is only believed when the TCP peer itself is a trusted
//! proxy, and the chain is read from the right: the first address that is not a
//! trusted proxy is the client. Reading from the left would let any client choose
//! its own address by sending the header, which defeats the login guard (it counts
//! failures per source address) and falsifies the audit log.

use std::net::{IpAddr, SocketAddr};

use axum::extract::ConnectInfo;
use http::HeaderMap;
use http::header::HeaderName;
use ipnet::IpNet;

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// The resolved client address of a request, stored in request extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    // `::ffff:10.0.0.1` must match `10.0.0.0/8`.
    let ip = ip.to_canonical();
    trusted.iter().any(|net| net.contains(&ip))
}

/// Resolves the client address from the TCP peer and `X-Forwarded-For`.
#[must_use]
pub fn resolve_client_ip(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    trusted: &[IpNet],
) -> Option<IpAddr> {
    let peer = peer?.to_canonical();
    if !is_trusted(peer, trusted) {
        return Some(peer);
    }
    let hops: Vec<&str> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    let mut current = peer;
    for hop in hops.into_iter().rev() {
        // A hop that does not parse ends the walk at the last address we could
        // vouch for: guessing past garbage would trust attacker-written text.
        let Ok(ip) = hop.parse::<IpAddr>() else {
            break;
        };
        current = ip.to_canonical();
        if !is_trusted(current, trusted) {
            break;
        }
    }
    Some(current)
}

/// The peer address recorded by the server for this connection.
#[must_use]
pub fn peer_ip(extensions: &http::Extensions) -> Option<IpAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|n| n.parse().expect("cidr")).collect()
    }

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(X_FORWARDED_FOR, v.parse().expect("header"));
        }
        h
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("ip")
    }

    #[test]
    fn without_a_peer_there_is_no_client() {
        assert_eq!(resolve_client_ip(None, &HeaderMap::new(), &[]), None);
    }

    #[test]
    fn must_not_trust_forwarded_for_from_an_untrusted_peer() {
        let trusted = nets(&["10.0.0.0/8"]);
        let resolved = resolve_client_ip(Some(ip("203.0.113.9")), &xff(&["1.2.3.4"]), &trusted);
        assert_eq!(resolved, Some(ip("203.0.113.9")));
        let resolved = resolve_client_ip(Some(ip("203.0.113.9")), &xff(&["1.2.3.4"]), &[]);
        assert_eq!(
            resolved,
            Some(ip("203.0.113.9")),
            "no proxies configured, header ignored"
        );
    }

    #[test]
    fn trusted_peer_hands_over_the_forwarded_client() {
        let trusted = nets(&["10.0.0.0/8"]);
        let resolved = resolve_client_ip(Some(ip("10.0.0.5")), &xff(&["198.51.100.7"]), &trusted);
        assert_eq!(resolved, Some(ip("198.51.100.7")));
    }

    #[test]
    fn chain_is_read_from_the_right_so_clients_cannot_spoof() {
        let trusted = nets(&["10.0.0.0/8"]);
        // The client sent "1.1.1.1"; the ingress appended the real address.
        let headers = xff(&["1.1.1.1, 198.51.100.7, 10.0.0.9"]);
        let resolved = resolve_client_ip(Some(ip("10.0.0.5")), &headers, &trusted);
        assert_eq!(resolved, Some(ip("198.51.100.7")));
    }

    #[test]
    fn several_headers_are_one_chain_in_order() {
        let trusted = nets(&["10.0.0.0/8"]);
        let headers = xff(&["1.1.1.1", "198.51.100.7, 10.0.0.9"]);
        let resolved = resolve_client_ip(Some(ip("10.0.0.5")), &headers, &trusted);
        assert_eq!(resolved, Some(ip("198.51.100.7")));
    }

    #[test]
    fn all_trusted_chain_ends_at_the_leftmost_hop() {
        let trusted = nets(&["10.0.0.0/8"]);
        let resolved = resolve_client_ip(
            Some(ip("10.0.0.5")),
            &xff(&["10.1.1.1, 10.2.2.2"]),
            &trusted,
        );
        assert_eq!(resolved, Some(ip("10.1.1.1")));
    }

    #[test]
    fn missing_header_from_trusted_peer_yields_the_peer() {
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.5")), &HeaderMap::new(), &trusted),
            Some(ip("10.0.0.5"))
        );
    }

    #[test]
    fn garbage_hop_stops_the_walk_at_the_last_vouched_address() {
        let trusted = nets(&["10.0.0.0/8"]);
        let headers = xff(&["9.9.9.9, not-an-ip, 10.0.0.9"]);
        let resolved = resolve_client_ip(Some(ip("10.0.0.5")), &headers, &trusted);
        assert_eq!(resolved, Some(ip("10.0.0.9")));
        let headers = xff(&["garbage"]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.5")), &headers, &trusted),
            Some(ip("10.0.0.5"))
        );
    }

    #[test]
    fn ipv4_mapped_addresses_match_ipv4_networks() {
        let trusted = nets(&["10.0.0.0/8"]);
        let resolved = resolve_client_ip(
            Some(ip("::ffff:10.0.0.5")),
            &xff(&["198.51.100.7"]),
            &trusted,
        );
        assert_eq!(resolved, Some(ip("198.51.100.7")));
    }

    #[test]
    fn ipv6_clients_are_supported() {
        let trusted = nets(&["fd00::/8"]);
        let resolved = resolve_client_ip(Some(ip("fd00::1")), &xff(&["2001:db8::7"]), &trusted);
        assert_eq!(resolved, Some(ip("2001:db8::7")));
    }

    #[test]
    fn peer_ip_reads_connect_info() {
        let mut extensions = http::Extensions::new();
        assert_eq!(peer_ip(&extensions), None);
        extensions.insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 4000))));
        assert_eq!(peer_ip(&extensions), Some(ip("192.0.2.1")));
    }
}
