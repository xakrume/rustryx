//! Is this request HTTPS, and does its `Origin` belong to us.
//!
//! TLS ends in front of the node, so "HTTPS" is what a trusted proxy says. The rule is the one
//! `X-Forwarded-For` follows: the headers are believed only when the direct peer is in
//! `server.trusted_proxies`, and the right-most value, the one the proxy directly in front of
//! the node wrote, is the one that counts. A client cannot make a plain HTTP request look secure
//! by sending the header itself: either its peer address is not trusted (header ignored) or the
//! proxy appended its own observation to the right of whatever the client sent.

use http::HeaderMap;
use http::header::{HOST, HeaderName, ORIGIN};
use http::request::Parts;
use ipnet::IpNet;

use crate::client_ip::peer_ip;

const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
const FORWARDED: HeaderName = HeaderName::from_static("forwarded");

/// The right-most `X-Forwarded-Proto` value, lower-cased. `None` when the header is absent or
/// unreadable (an unreadable header is not evidence of HTTPS).
fn forwarded_proto_header(headers: &HeaderMap) -> Option<String> {
    let last = headers.get_all(X_FORWARDED_PROTO).iter().next_back()?;
    let text = last.to_str().ok()?;
    let value = text.rsplit(',').next()?.trim();
    (!value.is_empty()).then(|| value.to_ascii_lowercase())
}

/// The `proto=` of the right-most element of `Forwarded` (RFC 7239), lower-cased.
fn forwarded_proto_rfc7239(headers: &HeaderMap) -> Option<String> {
    let last = headers.get_all(FORWARDED).iter().next_back()?;
    let element = last.to_str().ok()?.rsplit(',').next()?;
    element.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("proto")
            .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
    })
}

/// True when the request arrived over HTTPS as far as a trusted proxy can tell.
///
/// Both headers are consulted when both are present and they must agree: one saying `http`
/// is enough to refuse, because the conservative reading of a contradiction is "not secure".
#[must_use]
pub(crate) fn is_https(parts: &Parts, trusted_proxies: &[IpNet]) -> bool {
    let Some(peer) = peer_ip(&parts.extensions) else {
        return false;
    };
    let peer = peer.to_canonical();
    if !trusted_proxies.iter().any(|net| net.contains(&peer)) {
        return false;
    }
    let verdicts = [
        forwarded_proto_header(&parts.headers),
        forwarded_proto_rfc7239(&parts.headers),
    ];
    let mut seen = false;
    for verdict in verdicts.into_iter().flatten() {
        if verdict != "https" {
            return false;
        }
        seen = true;
    }
    seen
}

/// `scheme://authority` of a URL-ish text, lower-cased, without a default port, or `None`
/// when it does not look like an origin.
fn normalize_origin(text: &str) -> Option<String> {
    let (scheme, rest) = text.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let default_port = if scheme == "https" { ":443" } else { ":80" };
    let authority = authority
        .strip_suffix(default_port)
        .map_or(authority.as_str(), |bare| bare)
        .to_owned();
    Some(format!("{scheme}://{authority}"))
}

/// What counts as the service origin for the CSRF and login checks.
#[derive(Debug, Clone, Default)]
pub struct OriginPolicy {
    allowed: Vec<String>,
}

impl OriginPolicy {
    /// The service origin (`server.base_url`) plus the CORS origins. An entry that is not an
    /// origin is dropped; with no usable entry the `Host` header decides (see [`Self::permits`]).
    #[must_use]
    pub fn new(service_origin: Option<&str>, cors_origins: &[String]) -> Self {
        let allowed = service_origin
            .into_iter()
            .chain(cors_origins.iter().map(String::as_str))
            .filter_map(normalize_origin)
            .collect();
        Self { allowed }
    }

    /// True when the `Origin` header of the request names the service. A missing, repeated,
    /// `null` or foreign `Origin` is refused.
    ///
    /// With a configured origin list the match is exact on scheme, host and port. Without one
    /// (a development node with no `server.base_url`) the `Origin` authority must equal the
    /// `Host` header: a cross-site request carries the attacker's origin next to the victim's
    /// host, so the comparison still separates them, but a proxy that rewrites `Host` breaks
    /// it, which is why production sets `server.base_url`.
    #[must_use]
    pub fn permits(&self, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(ORIGIN).iter();
        let (Some(origin), None) = (origins.next(), origins.next()) else {
            return false;
        };
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        let Some(origin) = normalize_origin(origin) else {
            return false;
        };
        if !self.allowed.is_empty() {
            return self.allowed.contains(&origin);
        }
        let Some(host) = headers.get(HOST).and_then(|h| h.to_str().ok()) else {
            return false;
        };
        let authority = origin
            .split_once("://")
            .map_or(origin.as_str(), |(_, authority)| authority);
        let host = host.trim().to_ascii_lowercase();
        let host = host
            .strip_suffix(":443")
            .or_else(|| host.strip_suffix(":80"))
            .unwrap_or(&host);
        authority == host
    }
}

#[cfg(test)]
mod tests;
