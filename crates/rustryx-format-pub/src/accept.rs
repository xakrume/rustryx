//! Content negotiation of the JSON routes.
//!
//! The specification asks clients to send `Accept: application/vnd.pub.v2+json` and servers to
//! treat a missing header as version 2, so future versions can change the answers without
//! breaking old clients. This repository speaks version 2 only. A request that names other types
//! and none that version 2 satisfies (a client of version 3) gets 406 instead of an answer it
//! would misread. Media ranges that include version 2 (`*/*`, `application/*`, the plain
//! `application/json` that tools such as curl and browsers send) are accepted; quality values
//! are not interpreted, because there is only one representation to choose.

use http::{HeaderMap, header};

use crate::error::MEDIA_TYPE;

/// True when the request can be answered with `application/vnd.pub.v2+json`.
#[must_use]
pub fn accepts_v2(headers: &HeaderMap) -> bool {
    let mut seen = false;
    for value in headers.get_all(header::ACCEPT) {
        // A header that is not text cannot name a type we serve, but it is not a reason to
        // refuse a client that sent it next to a good one, so it is skipped and counted as absent.
        let Ok(text) = value.to_str() else { continue };
        for range in text.split(',') {
            let media = range
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if media.is_empty() {
                continue;
            }
            seen = true;
            if media == MEDIA_TYPE
                || media == "application/json"
                || media == "application/*"
                || media == "*/*"
            {
                return true;
            }
        }
    }
    // No Accept header, or none with a usable value: version 2.
    !seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(header::ACCEPT, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn a_missing_header_means_version_2() {
        assert!(accepts_v2(&HeaderMap::new()));
        assert!(accepts_v2(&headers(&[""])));
        assert!(accepts_v2(&headers(&[" , "])));
    }

    #[test]
    fn version_2_and_the_ranges_that_include_it_are_accepted() {
        for value in [
            "application/vnd.pub.v2+json",
            "Application/VND.PUB.V2+JSON; q=0.9",
            "application/json",
            "application/*",
            "*/*",
            "text/html, application/xhtml+xml, */*;q=0.8",
            "application/vnd.pub.v3+json, application/vnd.pub.v2+json",
        ] {
            assert!(accepts_v2(&headers(&[value])), "{value}");
        }
        assert!(accepts_v2(&headers(&["text/plain", "*/*"])));
    }

    #[test]
    fn other_versions_alone_are_not_acceptable() {
        for value in [
            "application/vnd.pub.v3+json",
            "text/html",
            "application/vnd.pub.v1+json, text/plain",
        ] {
            assert!(!accepts_v2(&headers(&[value])), "{value}");
        }
    }
}
