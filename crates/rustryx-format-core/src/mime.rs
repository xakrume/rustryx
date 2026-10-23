//! Content-type guessing and sanitizing of client-declared content types.

/// Content type used when nothing better is known.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// Longest declared content type that is accepted at all.
const MAX_DECLARED_LEN: usize = 255;

/// Types a browser may execute or render with scripts. A client-declared type from
/// this list is never trusted for stored content: the stored type is guessed from
/// the extension instead. Downloads additionally carry `Content-Security-Policy:
/// sandbox` and `nosniff`, so this list is the second line of defence.
const ACTIVE_TYPES: [&str; 6] = [
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "text/xml",
    "application/xml",
    "text/javascript",
];

/// Generic types that carry no information; the extension is a better hint.
const GENERIC_TYPES: [&str; 3] = [
    OCTET_STREAM,
    "application/x-www-form-urlencoded",
    "binary/octet-stream",
];

/// Guesses a content type from the file extension of `path` (last segment).
///
/// Compound extensions (`.tar.gz`) are checked first. Unknown extensions give
/// `application/octet-stream`.
#[must_use]
pub fn guess_content_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    for (suffix, mime) in COMPOUND {
        if name.ends_with(suffix) {
            return mime;
        }
    }
    let Some((_, ext)) = name.rsplit_once('.') else {
        return OCTET_STREAM;
    };
    for (known, mime) in SIMPLE {
        if ext == known {
            return mime;
        }
    }
    OCTET_STREAM
}

const COMPOUND: [(&str, &str); 3] = [
    (".tar.gz", "application/gzip"),
    (".tar.bz2", "application/x-bzip2"),
    (".tar.xz", "application/x-xz"),
];

const SIMPLE: [(&str, &str); 34] = [
    ("jar", "application/java-archive"),
    ("war", "application/java-archive"),
    ("ear", "application/java-archive"),
    ("aar", "application/zip"),
    ("zip", "application/zip"),
    ("whl", "application/zip"),
    ("egg", "application/zip"),
    ("nupkg", "application/zip"),
    ("crate", "application/gzip"),
    ("tgz", "application/gzip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("pom", "application/xml"),
    ("xml", "application/xml"),
    ("json", "application/json"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("js", "text/javascript"),
    ("sha1", "text/plain"),
    ("sha256", "text/plain"),
    ("sha512", "text/plain"),
    ("md5", "text/plain"),
    ("asc", "text/plain"),
    ("sig", "application/octet-stream"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("svg", "image/svg+xml"),
    ("exe", "application/vnd.microsoft.portable-executable"),
];

/// Accepts a content type from a trusted-for-syntax source (an upstream response) if it is a
/// well-formed `type/subtype[; parameters]` of sane length. Unlike [`resolve_content_type`]
/// it does not drop generic or active types: it is used for cached index documents whose
/// served type is chosen by the format, not by this value.
#[must_use]
pub fn well_formed(declared: &str) -> Option<String> {
    let declared = declared.trim();
    if declared.is_empty()
        || declared.len() > MAX_DECLARED_LEN
        || declared.chars().any(char::is_control)
    {
        return None;
    }
    let essence = declared.split(';').next().unwrap_or(declared).trim();
    let (kind, subtype) = essence.split_once('/')?;
    (token_ok(kind) && token_ok(subtype)).then(|| declared.to_owned())
}

fn token_ok(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'+' | b'_'))
}

/// Picks the content type to store for an upload.
///
/// A declared type is used only when it is a well-formed `type/subtype`, short, not
/// generic and not an active (browser-executable) type. Otherwise the type is
/// guessed from `path`. The parameters part (`; charset=...`) is kept for text types.
#[must_use]
pub fn resolve_content_type(declared: Option<&str>, path: &str) -> String {
    match declared.and_then(sanitize_declared) {
        Some(declared) => declared,
        None => guess_content_type(path).to_owned(),
    }
}

fn sanitize_declared(declared: &str) -> Option<String> {
    let declared = well_formed(declared)?;
    let essence = declared
        .split(';')
        .next()
        .unwrap_or(&declared)
        .trim()
        .to_ascii_lowercase();
    let kind = essence.split_once('/').map(|(kind, _)| kind)?;
    if kind == "multipart" || GENERIC_TYPES.contains(&essence.as_str()) {
        return None;
    }
    if ACTIVE_TYPES.contains(&essence.as_str()) {
        return None;
    }
    Some(declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_common_extensions() {
        for (path, expected) in [
            ("com/acme/lib-1.0.jar", "application/java-archive"),
            ("pkg-1.0.tar.gz", "application/gzip"),
            ("PKG-1.0.TGZ", "application/gzip"),
            ("a/b/lib.pom", "application/xml"),
            ("x/index.json", "application/json"),
            ("x/readme.md", "text/markdown"),
            ("x/file.sha1", "text/plain"),
            ("x/noext", OCTET_STREAM),
            ("x/weird.unknownext", OCTET_STREAM),
            ("pkg.tar.xz", "application/x-xz"),
            ("pkg.tar.bz2", "application/x-bzip2"),
        ] {
            assert_eq!(guess_content_type(path), expected, "{path}");
        }
    }

    #[test]
    fn declared_type_is_used_when_specific() {
        assert_eq!(
            resolve_content_type(Some("application/vnd.pub.v2+json"), "x"),
            "application/vnd.pub.v2+json"
        );
        assert_eq!(
            resolve_content_type(Some("text/plain; charset=utf-8"), "x.bin"),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn generic_or_malformed_declared_type_falls_back_to_guess() {
        for declared in [
            "application/octet-stream",
            "application/x-www-form-urlencoded",
            "multipart/form-data; boundary=x",
            "garbage",
            "",
            "a/b/c",
            "text/pl ain",
            "/json",
        ] {
            assert_eq!(
                resolve_content_type(Some(declared), "lib.jar"),
                "application/java-archive",
                "{declared:?}"
            );
        }
        assert_eq!(
            resolve_content_type(None, "lib.jar"),
            "application/java-archive"
        );
    }

    #[test]
    fn must_not_store_active_content_types_declared_by_client() {
        for declared in [
            "text/html",
            "TEXT/HTML; charset=utf-8",
            "image/svg+xml",
            "application/xhtml+xml",
            "text/javascript",
        ] {
            assert_eq!(
                resolve_content_type(Some(declared), "payload.bin"),
                OCTET_STREAM,
                "{declared}"
            );
        }
    }

    #[test]
    fn well_formed_checks_syntax_only() {
        assert_eq!(
            well_formed("text/html; charset=utf-8").as_deref(),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            well_formed(" application/vnd.pub.v2+json ").as_deref(),
            Some("application/vnd.pub.v2+json")
        );
        for bad in ["", "x", "a/b/c", "text/pl ain", "a/b\nc"] {
            assert!(well_formed(bad).is_none(), "{bad:?}");
        }
        assert!(well_formed(&format!("text/{}", "a".repeat(300))).is_none());
    }

    #[test]
    fn rejects_control_characters_and_overlong_values() {
        assert_eq!(
            resolve_content_type(Some("text/plain\r\nX-Evil: 1"), "a"),
            OCTET_STREAM
        );
        let long = format!("text/{}", "a".repeat(300));
        assert_eq!(resolve_content_type(Some(&long), "a"), OCTET_STREAM);
    }
}
