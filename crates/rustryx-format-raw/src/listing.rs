//! Directory listings: `GET <dir>/` as HTML, or as paged JSON with `Accept: application/json`.
//!
//! A raw repository stores flat paths; a directory exists exactly while some file has its
//! prefix. The listing collapses the files below the requested prefix into their immediate
//! children: a file `a/b.txt` below `a/` is the entry `b.txt`, a file `a/d/x` below `a/` is the
//! directory entry `d`.
//!
//! * **Order** is the bytewise order of the storage path (the order `AssetRepo::list` has on
//!   both implementations), so files and directories interleave by name.
//! * **Paging.** A page is made of whole storage pages of `limit` files. The continuation
//!   token carries the opaque token of the asset port (the position after the last file that
//!   was looked at) and the last directory that was emitted, so a directory that spans two
//!   storage pages is not listed twice. Consequently a page holds at least `limit` entries
//!   unless the directory ends, and fewer than `2 * limit`; a page can also be shorter or
//!   empty with a token when a directory of unreadable files had to be skipped.
//! * **Scan bound.** One request looks at no more than [`MAX_SCANNED`] files (tunable with
//!   `RawHandler::with_scan_limit`); it then returns what it has with a token.
//! * **Authorization.** The engine authorized `browse` on the directory. Every file that
//!   contributes an entry is checked for `read` on its own path, so a content selector that
//!   allows only part of a directory shows only that part, and a directory entry appears only
//!   when a readable file lies below it. A directory in which nothing is readable answers 404
//!   like a directory that does not exist: the two must not be told apart.
//! * **Token.** The token names the directory it was issued for and is refused anywhere else.
//!   It is not a secret and grants nothing: the same checks run on every page.
//! * **Rendering.** All names are escaped for HTML and percent-encoded in links, and a link
//!   never starts with a scheme-like segment, so a file called `javascript:alert(1)` is text.

use std::fmt::Write as _;

use axum_core::body::Body;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderValue, Method, Response, header};
use rustryx_core::error::AuthError;
use rustryx_core::identity::Action;
use rustryx_core::model::Asset;
use rustryx_core::ports::{Decision, PageRequest, ResourceRef};
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError, NormalizedPath};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::accept::prefers_json;

/// Default number of files per storage page.
pub const DEFAULT_LIMIT: usize = 200;
/// Largest `limit` a client may ask for.
pub const MAX_LIMIT: usize = 1000;
/// Most files one request looks at.
pub const MAX_SCANNED: usize = 5000;
/// Longest continuation token accepted.
const MAX_TOKEN_LEN: usize = 2048;

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A stored file.
    File {
        /// Name below the listed directory.
        name: String,
        /// Size in bytes.
        size: u64,
        /// Last change.
        modified: OffsetDateTime,
        /// Stored content type.
        content_type: String,
        /// SHA-256 of the content, lowercase hex.
        sha256: String,
    },
    /// A directory that has at least one readable file below it.
    Directory {
        /// Name below the listed directory.
        name: String,
    },
}

/// A rendered page of a listing.
#[derive(Debug, Clone)]
pub struct Listing {
    /// Name of the repository the client addressed.
    pub repository: String,
    /// The directory, without leading slash and with a trailing slash; empty for the root.
    pub path: String,
    /// The entries of this page.
    pub entries: Vec<Entry>,
    /// Token of the next page, when there is one.
    pub next: Option<String>,
    /// The `limit` this page was requested with, repeated in the link to the next page.
    pub limit: usize,
}

/// Continuation state, serialized into the token.
#[derive(Debug, Serialize, Deserialize)]
struct Token {
    /// The directory prefix the token was issued for.
    p: String,
    /// The position token of the asset port.
    a: Option<String>,
    /// The last directory entry that was emitted.
    d: Option<String>,
}

impl Token {
    fn encode(&self) -> String {
        // A struct of strings always serializes.
        let json = serde_json::to_vec(self).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(json)
    }

    fn decode(text: &str, prefix: &str) -> Result<Self, FormatError> {
        let invalid = || FormatError::bad_request("invalid token");
        if text.len() > MAX_TOKEN_LEN {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
        let token: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if token.p != prefix {
            return Err(invalid());
        }
        Ok(token)
    }
}

/// Maps a failed authorization lookup to the client error, as the engine does: capacity
/// problems are retryable, everything else fails closed.
pub(crate) fn map_auth_error(error: &AuthError) -> FormatError {
    if matches!(error, AuthError::Overloaded) {
        return FormatError::Unavailable {
            reason: "authorization capacity exhausted, retry",
            retry_after: Some(std::time::Duration::from_secs(1)),
        };
    }
    // Nothing is logged here: the engine logs the final error once, and a failing provider
    // must not produce one line per listed file.
    FormatError::Forbidden
}

fn parse_limit(req: &ArtifactRequest) -> Result<usize, FormatError> {
    let Some(text) = req.head.query_param("limit") else {
        return Ok(DEFAULT_LIMIT);
    };
    match text.parse::<usize>() {
        Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        _ => Err(FormatError::bad_request("invalid limit")),
    }
}

/// The storage prefix of a directory: empty for the root, `a/b/` otherwise.
fn prefix_of(path: &NormalizedPath) -> String {
    if path.is_root() {
        String::new()
    } else {
        format!("{}/", path.as_str())
    }
}

async fn readable(cx: &FormatContext, path: &str) -> Result<bool, FormatError> {
    let repository = cx.repository();
    let resource = ResourceRef::for_repository(repository, path);
    match cx
        .ports()
        .rbac
        .check(cx.principal(), Action::Read, &resource)
        .await
    {
        Ok(Decision::Allow) => Ok(true),
        Ok(Decision::Deny) => Ok(false),
        Err(error) => Err(map_auth_error(&error)),
    }
}

struct Scan {
    entries: Vec<Entry>,
    next: Option<String>,
    last_dir: Option<String>,
}

async fn scan(
    cx: &FormatContext,
    prefix: &str,
    limit: usize,
    max_scanned: usize,
    token: Option<Token>,
) -> Result<Scan, FormatError> {
    let repository = cx.repository();
    let (mut after, mut last_dir) = token.map_or((None, None), |t| (t.a, t.d));
    let mut entries = Vec::new();
    let mut scanned = 0usize;
    loop {
        let request = PageRequest {
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
            after: after.take(),
        };
        let page = cx
            .ports()
            .assets
            .list(repository.id, prefix, &request)
            .await?;
        let empty = page.items.is_empty();
        scanned += page.items.len();
        for asset in page.items {
            collect(cx, prefix, asset, &mut entries, &mut last_dir).await?;
        }
        after = page.next;
        if empty || after.is_none() || entries.len() >= limit || scanned >= max_scanned {
            break;
        }
    }
    Ok(Scan {
        entries,
        next: after,
        last_dir,
    })
}

async fn collect(
    cx: &FormatContext,
    prefix: &str,
    asset: Asset,
    entries: &mut Vec<Entry>,
    last_dir: &mut Option<String>,
) -> Result<(), FormatError> {
    // The port promises the prefix; a row outside it is skipped rather than shown.
    let Some(relative) = asset.path.strip_prefix(prefix) else {
        return Ok(());
    };
    if relative.is_empty() {
        return Ok(());
    }
    match relative.split_once('/') {
        None => {
            if readable(cx, &asset.path).await? {
                entries.push(Entry::File {
                    name: relative.to_owned(),
                    size: asset.size_bytes,
                    modified: asset.updated_at,
                    content_type: asset.content_type.clone(),
                    sha256: asset.checksums.sha256.clone(),
                });
            }
        }
        Some((directory, _)) => {
            // Files of one directory are adjacent in path order: once it is listed, the rest
            // of its files add nothing.
            if last_dir.as_deref() == Some(directory) {
                return Ok(());
            }
            if readable(cx, &asset.path).await? {
                entries.push(Entry::Directory {
                    name: directory.to_owned(),
                });
                *last_dir = Some(directory.to_owned());
            }
        }
    }
    Ok(())
}

/// Serves the listing of the directory `req` addresses, looking at no more than `max_scanned`
/// files ([`MAX_SCANNED`] unless the handler was configured otherwise).
pub(crate) async fn serve(
    cx: &FormatContext,
    req: &ArtifactRequest,
    max_scanned: usize,
) -> Result<Response<Body>, FormatError> {
    let limit = parse_limit(req)?;
    let prefix = prefix_of(&req.path);
    let token = req
        .head
        .query_param("token")
        .map(|text| Token::decode(&text, &prefix))
        .transpose()?;
    let scanned = scan(cx, &prefix, limit, max_scanned, token).await?;
    if scanned.entries.is_empty() && scanned.next.is_none() && !req.path.is_root() {
        // Nothing readable: indistinguishable from a directory that does not exist.
        return Err(FormatError::NotFound);
    }
    let next = scanned.next.map(|after| {
        Token {
            p: prefix.clone(),
            a: Some(after),
            d: scanned.last_dir,
        }
        .encode()
    });
    let listing = Listing {
        repository: cx.origin_repository().name.to_string(),
        path: prefix,
        entries: scanned.entries,
        next,
        limit,
    };
    Ok(render(req, &listing))
}

/// Renders a page in the representation the caller prefers.
#[must_use]
pub fn render(req: &ArtifactRequest, listing: &Listing) -> Response<Body> {
    let (content_type, text) = if prefers_json(&req.head.headers) {
        ("application/json", render_json(listing))
    } else {
        ("text/html; charset=utf-8", render_html(listing))
    };
    let length = text.len();
    let body = if req.head.method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(text)
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // The page has no script and no external resource: nothing is allowed to load, and the
    // inline style is the only exception.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    // A listing changes with every upload and depends on who asks.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    response
}

fn render_json(listing: &Listing) -> String {
    let items: Vec<serde_json::Value> = listing
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::File {
                name,
                size,
                modified,
                content_type,
                sha256,
            } => serde_json::json!({
                "name": name,
                "type": "file",
                "size": size,
                "lastModified": modified.format(&Rfc3339).unwrap_or_default(),
                "contentType": content_type,
                "sha256": sha256,
            }),
            Entry::Directory { name } => serde_json::json!({ "name": name, "type": "directory" }),
        })
        .collect();
    serde_json::json!({
        "repository": listing.repository,
        "path": listing.path,
        "items": items,
        "continuationToken": listing.next,
    })
    .to_string()
}

fn render_html(listing: &Listing) -> String {
    let title = format!(
        "Index of /repository/{}/{}",
        listing.repository, listing.path
    );
    let mut html = String::with_capacity(1024 + listing.entries.len() * 160);
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>");
    html.push_str(&escape(&title));
    html.push_str("</title><style>");
    html.push_str(
        "body{font:14px/1.5 system-ui,sans-serif;margin:2rem}table{border-collapse:collapse}\
         td,th{padding:.2rem 1rem .2rem 0;text-align:left}td.n{text-align:right}",
    );
    html.push_str("</style></head><body><h1>");
    html.push_str(&escape(&title));
    html.push_str("</h1><table><thead><tr><th>Name</th><th>Size</th><th>Last modified</th></tr></thead><tbody>");
    if !listing.path.is_empty() {
        html.push_str("<tr><td><a href=\"../\">../</a></td><td></td><td></td></tr>");
    }
    for entry in &listing.entries {
        match entry {
            Entry::Directory { name } => {
                let _ = write!(
                    html,
                    "<tr><td><a href=\"./{}/\">{}/</a></td><td></td><td></td></tr>",
                    escape(&encode_segment(name)),
                    escape(name)
                );
            }
            Entry::File {
                name,
                size,
                modified,
                ..
            } => {
                let _ = write!(
                    html,
                    "<tr><td><a href=\"./{}\">{}</a></td><td class=\"n\" title=\"{} bytes\">{}</td><td>{}</td></tr>",
                    escape(&encode_segment(name)),
                    escape(name),
                    size,
                    human_size(*size),
                    escape(&modified.format(&Rfc3339).unwrap_or_default())
                );
            }
        }
    }
    html.push_str("</tbody></table>");
    if let Some(next) = &listing.next {
        let _ = write!(
            html,
            "<p><a href=\"?limit={}&amp;token={}\">Next page</a></p>",
            listing.limit,
            escape(next)
        );
    }
    html.push_str("</body></html>\n");
    html
}

/// Percent-encodes one path segment for a link (everything but unreserved characters), so a
/// name can neither start a scheme nor end an attribute.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the value is a display figure with one decimal; a file size beyond 2^52 bytes does not need exact digits"
)]
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn listing(entries: Vec<Entry>) -> Listing {
        Listing {
            repository: "files".into(),
            path: "docs/".into(),
            entries,
            next: None,
            limit: 200,
        }
    }

    fn file(name: &str) -> Entry {
        Entry::File {
            name: name.into(),
            size: 1536,
            modified: datetime!(2026-10-24 10:00:00 UTC),
            content_type: "text/plain".into(),
            sha256: "ab".repeat(32),
        }
    }

    #[test]
    fn tokens_round_trip_and_are_bound_to_their_directory() {
        let token = Token {
            p: "a/".into(),
            a: Some("cursor".into()),
            d: Some("sub".into()),
        };
        let text = token.encode();
        let back = Token::decode(&text, "a/").unwrap();
        assert_eq!(
            (back.a.as_deref(), back.d.as_deref()),
            (Some("cursor"), Some("sub"))
        );
        assert!(Token::decode(&text, "b/").is_err(), "another directory");
        assert!(Token::decode(&text, "").is_err());
    }

    #[test]
    fn broken_tokens_are_a_client_error() {
        for text in ["", "!!!", "e30", &"A".repeat(MAX_TOKEN_LEN + 1)] {
            assert!(
                matches!(Token::decode(text, ""), Err(FormatError::BadRequest(_))),
                "{text:.20}"
            );
        }
    }

    #[test]
    fn json_listing_has_the_documented_shape() {
        let mut page = listing(vec![file("a.txt"), Entry::Directory { name: "sub".into() }]);
        page.next = Some("tok".into());
        let value: serde_json::Value = serde_json::from_str(&render_json(&page)).unwrap();
        assert_eq!(value["repository"], "files");
        assert_eq!(value["path"], "docs/");
        assert_eq!(value["continuationToken"], "tok");
        assert_eq!(value["items"][0]["type"], "file");
        assert_eq!(value["items"][0]["size"], 1536);
        assert_eq!(value["items"][0]["lastModified"], "2026-10-24T10:00:00Z");
        assert_eq!(value["items"][1]["type"], "directory");
        assert!(value["items"][1].get("size").is_none());
        let last: serde_json::Value = serde_json::from_str(&render_json(&listing(vec![]))).unwrap();
        assert!(last["continuationToken"].is_null());
    }

    #[test]
    fn must_not_let_a_file_name_inject_markup_or_a_script_link() {
        let hostile = [
            "<script>alert(1)</script>.txt",
            "\"><img src=x onerror=alert(1)>",
            "javascript:alert(1)",
            "a'b&c<d>.txt",
        ];
        let html = render_html(&listing(hostile.iter().map(|n| file(n)).collect()));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("href=\"./javascript:"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;.txt"));
        assert!(html.contains("javascript%3Aalert%281%29"));
        // Every anchor of a name is relative and starts with ./
        for line in html.split("<tr>").skip(3) {
            assert!(line.contains("href=\"./"), "{line}");
        }
    }

    #[test]
    fn the_title_and_the_next_link_are_escaped_too() {
        let mut page = listing(vec![]);
        page.repository = "r<x>".into();
        page.path = "a&b/".into();
        page.next = Some("t\"1".into());
        let html = render_html(&page);
        assert!(html.contains("Index of /repository/r&lt;x&gt;/a&amp;b/"));
        assert!(html.contains("token=t&quot;1"));
        assert!(!html.contains("r<x>"));
    }

    #[test]
    fn the_root_has_no_parent_link_and_a_subdirectory_has() {
        let mut page = listing(vec![]);
        page.path = String::new();
        assert!(!render_html(&page).contains("href=\"../\""));
        page.path = "docs/".into();
        assert!(render_html(&page).contains("href=\"../\""));
    }

    #[test]
    fn sizes_are_human_readable_and_exact_in_the_title() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
        assert!(human_size(u64::MAX).ends_with("TiB"));
        let html = render_html(&listing(vec![file("a")]));
        assert!(html.contains("title=\"1536 bytes\""));
        assert!(html.contains("1.5 KiB"));
    }

    #[test]
    fn segment_encoding_keeps_only_unreserved_characters() {
        assert_eq!(encode_segment("a b+c:d/é"), "a%20b%2Bc%3Ad%2F%C3%A9");
        assert_eq!(encode_segment("A-z_0.9~"), "A-z_0.9~");
    }
}
