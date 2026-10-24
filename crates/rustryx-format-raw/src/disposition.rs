//! Response hardening for stored content: `Content-Disposition` for active types and the
//! headers every successful raw response must carry.
//!
//! Why here and not in the shared pipeline: the pipeline serves a stored file as it was
//! stored (it already adds `nosniff` and a sandboxing `Content-Security-Policy`), and what a
//! repository does about html it hosts is a decision of the raw format and its
//! `format_config`. The hosted, proxy and group paths all return a `Response`, so one
//! post-processing step covers the three of them and a member of a group.

use std::fmt::Write as _;

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};

use crate::config::{ContentDisposition, RawConfig};

/// Applies the repository's delivery policy to the response of a file read.
///
/// * every successful response gets `X-Content-Type-Options: nosniff` (it is only added when
///   missing, the pipeline normally has set it already);
/// * a `200` or `206` whose content type a browser would execute gets
///   `Content-Disposition: attachment; filename=...` unless the repository is `inline`.
pub(crate) fn harden(response: &mut Response<Body>, config: RawConfig, file_name: Option<&str>) {
    let status = response.status();
    if !status.is_success() {
        return;
    }
    response
        .headers_mut()
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    if !matches!(status, StatusCode::OK | StatusCode::PARTIAL_CONTENT) {
        return;
    }
    if config.content_disposition == ContentDisposition::Inline
        || response.headers().contains_key(header::CONTENT_DISPOSITION)
    {
        return;
    }
    let active = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_active_content_type);
    if active {
        let value = attachment_value(file_name.unwrap_or("download"));
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
}

/// True when a browser renders the type as a document that can run script: html, xhtml, svg,
/// every xml flavour (an xml document in the xhtml or svg namespace executes script), the
/// script types and xslt.
#[must_use]
pub fn is_active_content_type(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "text/html"
            | "application/xhtml+xml"
            | "image/svg+xml"
            | "text/xml"
            | "application/xml"
            | "text/javascript"
            | "application/javascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/xsl"
            | "application/xslt+xml"
            | "text/cache-manifest"
    ) || essence.ends_with("+xml")
}

/// `attachment; filename="<ascii fallback>"; filename*=UTF-8''<percent encoded name>`
/// (RFC 6266, RFC 8187). The fallback keeps only characters that are safe inside a quoted
/// string, so a hostile file name cannot break out of the header value.
fn attachment_value(name: &str) -> HeaderValue {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '+' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    // Both parts are built from characters that are valid in a header value, so this does
    // not fail; the plain form is the answer to a case that cannot happen.
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, content_type: &str) -> Response<Body> {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::from_u16(status).unwrap();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).unwrap(),
        );
        response
    }

    fn disposition(response: &Response<Body>) -> Option<&str> {
        response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
    }

    #[test]
    fn active_types_are_recognized_in_any_spelling() {
        for active in [
            "text/html",
            "TEXT/HTML; charset=utf-8",
            "application/xhtml+xml",
            "image/svg+xml",
            "application/xml",
            "text/xml",
            "application/atom+xml",
            "application/rdf+xml",
            "text/javascript",
            "application/javascript; charset=utf-8",
            "application/xslt+xml",
        ] {
            assert!(is_active_content_type(active), "{active}");
        }
        for inert in [
            "text/plain",
            "application/json",
            "application/octet-stream",
            "application/gzip",
            "image/png",
            "application/pdf",
            "text/css",
            "",
        ] {
            assert!(!is_active_content_type(inert), "{inert}");
        }
    }

    #[test]
    fn must_not_serve_stored_html_inline_by_default() {
        for kind in [
            "text/html",
            "image/svg+xml",
            "application/xml",
            "text/javascript",
        ] {
            let mut r = response(200, kind);
            harden(&mut r, RawConfig::default(), Some("page.html"));
            let value = disposition(&r).expect("attachment");
            assert!(value.starts_with("attachment;"), "{kind}: {value}");
            assert!(value.contains("filename=\"page.html\""));
            assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        }
    }

    #[test]
    fn a_partial_response_of_active_content_is_an_attachment_too() {
        let mut r = response(206, "text/html");
        harden(&mut r, RawConfig::default(), Some("a.html"));
        assert!(disposition(&r).is_some());
    }

    #[test]
    fn inert_content_gets_no_disposition() {
        let mut r = response(200, "application/gzip");
        harden(&mut r, RawConfig::default(), Some("a.tgz"));
        assert!(disposition(&r).is_none());
        assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }

    #[test]
    fn an_inline_repository_serves_html_without_a_disposition_but_keeps_nosniff() {
        let config = RawConfig {
            content_disposition: ContentDisposition::Inline,
        };
        let mut r = response(200, "text/html");
        harden(&mut r, config, Some("page.html"));
        assert!(disposition(&r).is_none());
        assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }

    #[test]
    fn errors_and_not_modified_are_left_alone() {
        for status in [304, 404, 416, 500] {
            let mut r = response(status, "text/html");
            harden(&mut r, RawConfig::default(), Some("a.html"));
            assert!(disposition(&r).is_none(), "{status}");
        }
    }

    #[test]
    fn an_existing_disposition_is_kept() {
        let mut r = response(200, "text/html");
        r.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("inline"),
        );
        harden(&mut r, RawConfig::default(), Some("a.html"));
        assert_eq!(disposition(&r), Some("inline"));
    }

    #[test]
    fn a_hostile_file_name_cannot_break_out_of_the_header() {
        let value = attachment_value("\"; filename=evil.exe; x=\"\r\nSet-Cookie: a=b é.html");
        let text = value.to_str().unwrap();
        assert!(!text.contains('\r') && !text.contains('\n'));
        // The quoted fallback holds no quote of its own and no semicolon.
        let fallback = text
            .split("filename=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap();
        assert!(!fallback.contains([';', '"', '\\']));
        assert!(text.contains("filename*=UTF-8''"));
        assert!(text.contains("%C3%A9"));
    }

    #[test]
    fn the_fallback_name_is_used_when_the_path_has_no_file_name() {
        let mut r = response(200, "text/html");
        harden(&mut r, RawConfig::default(), None);
        assert!(disposition(&r).unwrap().contains("filename=\"download\""));
    }
}
