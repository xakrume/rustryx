//! The `WWW-Authenticate: Bearer realm="...",service="...",scope="..."` challenge of an
//! upstream registry (RFC 6750 section 3, with the registry token flow of
//! <https://distribution.github.io/distribution/spec/auth/token/>).
//!
//! The header is attacker influenced (a compromised upstream, or anything between us and a
//! plain HTTP upstream), so the parser is strict about size and shape and never panics:
//! at most 4 KiB, at most 16 parameters, quoted strings with `\` escapes, no parameter is
//! interpreted beyond being stored. What is done with the `realm` is the caller's business:
//! it is requested through the executor, so the SSRF policy decides whether the host may be
//! reached at all.

use url::Url;

/// Longest challenge header value read.
const MAX_CHALLENGE_LEN: usize = 4 * 1024;
/// Most parameters read.
const MAX_PARAMS: usize = 16;

/// A parsed bearer challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BearerChallenge {
    /// Where to ask for a token.
    pub(crate) realm: Url,
    /// The `service` to put in the token request, if the upstream named one.
    pub(crate) service: Option<String>,
}

/// The first bearer challenge among the values of the `WWW-Authenticate` headers.
pub(crate) fn find_bearer<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Option<BearerChallenge> {
    values.into_iter().find_map(parse_bearer)
}

/// Parses one header value as a bearer challenge.
pub(crate) fn parse_bearer(value: &str) -> Option<BearerChallenge> {
    if value.len() > MAX_CHALLENGE_LEN {
        return None;
    }
    let value = value.trim_start();
    let scheme_end = value
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(value.len());
    let (scheme, rest) = value.split_at(scheme_end);
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut realm = None;
    let mut service = None;
    for (key, parameter) in parameters(rest).into_iter().take(MAX_PARAMS) {
        match key.to_ascii_lowercase().as_str() {
            "realm" if realm.is_none() => realm = Some(parameter),
            "service" if service.is_none() => service = Some(parameter),
            _ => {}
        }
    }
    let realm = Url::parse(&realm?).ok()?;
    if !matches!(realm.scheme(), "http" | "https") || realm.host_str().is_none() {
        return None;
    }
    // Credentials in the URL would be sent along by nothing here, but they have no place in a
    // realm and the SSRF policy refuses them anyway: refuse early, with a clearer reason.
    if !realm.username().is_empty() || realm.password().is_some() {
        return None;
    }
    Some(BearerChallenge { realm, service })
}

/// `key=value` pairs separated by commas; a value is a token or a quoted string.
fn parameters(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars
            .next_if(|c| *c == ',' || c.is_ascii_whitespace())
            .is_some()
        {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && *c != ',' && !c.is_ascii_whitespace()) {
            key.push(c);
        }
        if key.is_empty() {
            break;
        }
        if chars.next_if_eq(&'=').is_none() {
            // A bare token (for example the `Bearer` of a second scheme) carries no value.
            continue;
        }
        let mut value = String::new();
        if chars.next_if_eq(&'"').is_some() {
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    other => value.push(other),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != ',' && !c.is_ascii_whitespace()) {
                value.push(c);
            }
        }
        out.push((key, value));
        if out.len() > MAX_PARAMS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests;
