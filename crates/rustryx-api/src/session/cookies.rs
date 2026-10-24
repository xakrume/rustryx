//! Reading the `Cookie` header and writing `Set-Cookie` values.

use http::HeaderMap;
use http::header::COOKIE;

/// Longest cookie value read. The session token is the largest and is capped at 16 KiB.
const MAX_COOKIE_VALUE: usize = 16 * 1024;

/// What looking for one cookie found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// No cookie of that name.
    Absent,
    /// Exactly one.
    One(String),
    /// More than one, or a value that is too long or not text: refused rather than guessed.
    Ambiguous,
}

/// Finds the cookie `name` in the `Cookie` header(s).
///
/// HTTP/2 clients may split the header into several fields, so every field is read. Two cookies
/// of the same name are ambiguous: a browser sends the more specific path first, and picking one
/// would let a planted cookie shadow the real one (the `__Host-` prefix already forbids planting
/// from a sibling host, this is the second line).
pub(crate) fn find(headers: &HeaderMap, name: &str) -> Found {
    let mut found: Option<String> = None;
    for field in headers.get_all(COOKIE) {
        let Ok(text) = field.to_str() else {
            return Found::Ambiguous;
        };
        for pair in text.split(';') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key.trim() != name {
                continue;
            }
            let value = value.trim();
            if value.len() > MAX_COOKIE_VALUE || found.is_some() {
                return Found::Ambiguous;
            }
            found = Some(value.to_owned());
        }
    }
    found.map_or(Found::Absent, Found::One)
}

/// The text of the `Cookie` header(s) joined with `; `, for the SSO adapter that reads its
/// own cookies from it.
pub(crate) fn header_text(headers: &HeaderMap) -> String {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ")
}

/// A `Set-Cookie` value for a cookie that lives for `max_age_secs`.
pub(crate) struct SetCookie<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub max_age_secs: i64,
    pub http_only: bool,
    /// `Lax` or `Strict`.
    pub same_site: &'static str,
}

impl SetCookie<'_> {
    /// `__Host-` cookies must be `Secure`, `Path=/` and carry no `Domain`: all three hold here
    /// for every cookie, whatever its name.
    pub(crate) fn render(&self) -> String {
        let mut out = format!(
            "{}={}; Path=/; Secure; SameSite={}; Max-Age={}",
            self.name,
            self.value,
            self.same_site,
            self.max_age_secs.max(0)
        );
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        out
    }
}

/// A `Set-Cookie` value that makes the browser drop the cookie.
pub(crate) fn clearing(name: &str, http_only: bool, same_site: &'static str) -> String {
    SetCookie {
        name,
        value: "",
        max_age_secs: 0,
        http_only,
        same_site,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(fields: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for field in fields {
            map.append(COOKIE, field.parse().expect("header"));
        }
        map
    }

    #[test]
    fn a_cookie_is_found_among_others_and_across_split_fields() {
        let h = headers(&["a=1; __Host-s=tok; b=2"]);
        assert_eq!(find(&h, "__Host-s"), Found::One("tok".into()));
        let split = headers(&["a=1", "__Host-s=tok"]);
        assert_eq!(find(&split, "__Host-s"), Found::One("tok".into()));
        assert_eq!(find(&h, "missing"), Found::Absent);
        assert_eq!(find(&HeaderMap::new(), "x"), Found::Absent);
    }

    #[test]
    fn a_similar_name_is_not_the_cookie() {
        let h = headers(&["x__Host-s=evil; __Host-s2=other"]);
        assert_eq!(find(&h, "__Host-s"), Found::Absent);
    }

    #[test]
    fn must_not_pick_one_of_two_cookies_with_the_same_name() {
        assert_eq!(find(&headers(&["s=1; s=2"]), "s"), Found::Ambiguous);
        assert_eq!(find(&headers(&["s=1", "s=2"]), "s"), Found::Ambiguous);
    }

    #[test]
    fn oversized_or_binary_values_are_ambiguous() {
        let big = format!("s={}", "a".repeat(MAX_COOKIE_VALUE + 1));
        assert_eq!(find(&headers(&[&big]), "s"), Found::Ambiguous);
        let mut map = HeaderMap::new();
        map.append(
            COOKIE,
            http::HeaderValue::from_bytes(b"s=\xff\xfe").expect("opaque bytes are allowed"),
        );
        assert_eq!(find(&map, "s"), Found::Ambiguous);
    }

    #[test]
    fn set_cookie_carries_the_host_prefix_requirements() {
        let rendered = SetCookie {
            name: "__Host-x",
            value: "v",
            max_age_secs: 60,
            http_only: true,
            same_site: "Lax",
        }
        .render();
        assert_eq!(
            rendered,
            "__Host-x=v; Path=/; Secure; SameSite=Lax; Max-Age=60; HttpOnly"
        );
        assert!(!rendered.contains("Domain"));
        let script_readable = SetCookie {
            name: "__Host-c",
            value: "v",
            max_age_secs: -5,
            http_only: false,
            same_site: "Strict",
        }
        .render();
        assert!(!script_readable.contains("HttpOnly"));
        assert!(script_readable.contains("Max-Age=0"), "never negative");
    }

    #[test]
    fn clearing_expires_the_cookie_at_once() {
        let cleared = clearing("__Host-x", true, "Lax");
        assert!(cleared.starts_with("__Host-x=;") && cleared.contains("Max-Age=0"));
    }

    #[test]
    fn the_header_text_joins_every_field() {
        assert_eq!(header_text(&headers(&["a=1", "b=2"])), "a=1; b=2");
        assert_eq!(header_text(&HeaderMap::new()), "");
    }
}
