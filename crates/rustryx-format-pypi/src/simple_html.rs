//! The HTML simple API (PEP 503, with the attributes of PEP 592, 658 and 714).
//!
//! Specification: <https://packaging.python.org/en/latest/specifications/simple-repository-api/>.
//! One anchor per file: the text is the file name, `href` the URL with a `#<algo>=<hex>`
//! fragment, `data-requires-python` the `Requires-Python` value (with `<` and `>` encoded),
//! `data-yanked` the yank reason (present and possibly empty), `data-core-metadata` and the
//! older `data-dist-info-metadata` the hash of the metadata file or `true`.
//!
//! The reader is a tolerant tokenizer, not an HTML parser: it finds `<a ...>text</a>` pairs,
//! skips comments, understands quoted, unquoted and valueless attributes and the entities that
//! can appear in names and URLs, and honours `<base href>`. Real indexes serve documents that
//! are not well formed XML, so strictness would turn an upstream quirk into an outage; the
//! bounds (document length, anchor count) are enforced instead.

use url::Url;

use crate::name::ProjectName;
use crate::page::{
    CoreMetadataRef, FileEntry, MAX_FILES_PER_PAGE, MAX_PROJECTS_PER_INDEX, ParseError,
    ProjectPage, RootPage, is_hex, preferred_hash,
};

fn escape_attribute(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_text(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn page_start(title: &str, out: &mut String) {
    out.push_str("<!DOCTYPE html>\n<html>\n  <head>\n    <meta name=\"pypi:repository-version\" content=\"1.1\">\n    <title>");
    escape_text(title, out);
    out.push_str("</title>\n  </head>\n  <body>\n    <h1>");
    escape_text(title, out);
    out.push_str("</h1>\n");
}

fn page_end(out: &mut String) {
    out.push_str("  </body>\n</html>\n");
}

fn push_attribute(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape_attribute(value, out);
    out.push('"');
}

/// Renders a project page as PEP 503 HTML.
#[must_use]
pub fn render_project(page: &ProjectPage) -> Vec<u8> {
    let mut out = String::with_capacity(256 + page.files.len() * 256);
    page_start(&format!("Links for {}", page.name), &mut out);
    for file in &page.files {
        out.push_str("    <a");
        let mut href = file.url.clone();
        if let Some((algo, hex)) = file.fragment_hash() {
            href.push('#');
            href.push_str(algo);
            href.push('=');
            href.push_str(hex);
        }
        push_attribute(&mut out, "href", &href);
        if let Some(requires) = &file.requires_python {
            push_attribute(&mut out, "data-requires-python", requires);
        }
        if let Some(reason) = &file.yanked {
            push_attribute(&mut out, "data-yanked", reason);
        }
        if let Some(reference) = &file.core_metadata {
            let value = match reference {
                CoreMetadataRef::Available => "true".to_owned(),
                CoreMetadataRef::Hashes(hashes) => preferred_hash(hashes)
                    .map_or_else(|| "true".to_owned(), |(algo, hex)| format!("{algo}={hex}")),
            };
            push_attribute(&mut out, "data-core-metadata", &value);
            push_attribute(&mut out, "data-dist-info-metadata", &value);
        }
        out.push('>');
        escape_text(&file.filename, &mut out);
        out.push_str("</a><br/>\n");
    }
    page_end(&mut out);
    out.into_bytes()
}

/// Renders the root index as PEP 503 HTML.
#[must_use]
pub fn render_root(root: &RootPage) -> Vec<u8> {
    let mut out = String::with_capacity(256 + root.projects.len() * 64);
    page_start("Simple index", &mut out);
    for name in &root.projects {
        out.push_str("    <a");
        push_attribute(&mut out, "href", &format!("{name}/"));
        out.push('>');
        escape_text(name, &mut out);
        out.push_str("</a><br/>\n");
    }
    page_end(&mut out);
    out.into_bytes()
}

/// Decodes the character references that occur in indexes: the five named ones and numeric
/// references. Anything else is left as it is.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        let tail = rest.get(at..).unwrap_or_default();
        let decoded = tail.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = tail.get(1..end)?;
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse::<u32>().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end + 1))
        });
        if let Some((c, consumed)) = decoded {
            out.push(c);
            rest = tail.get(consumed..).unwrap_or_default();
        } else {
            out.push('&');
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

struct Anchor {
    attributes: Vec<(String, String)>,
    text: String,
}

impl Anchor {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Parses the attributes of one tag, `text` being what lies between the tag name and `>`.
fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut attributes = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        while bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
        {
            at += 1;
        }
        let start = at;
        while bytes
            .get(at)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'=' && *b != b'/')
        {
            at += 1;
        }
        let name = text.get(start..at).unwrap_or_default().to_ascii_lowercase();
        if name.is_empty() {
            break;
        }
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            attributes.push((name, String::new()));
            continue;
        }
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let value = if let Some(quote @ (b'"' | b'\'')) = bytes.get(at) {
            at += 1;
            let start = at;
            while bytes.get(at).is_some_and(|b| b != quote) {
                at += 1;
            }
            let value = text.get(start..at).unwrap_or_default();
            at += 1;
            value
        } else {
            let start = at;
            while bytes.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
                at += 1;
            }
            text.get(start..at).unwrap_or_default()
        };
        attributes.push((name, decode_entities(value)));
    }
    attributes
}

/// End of the tag that starts at `from` (the index of `>`), skipping quoted attribute values.
fn tag_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    let mut quote: Option<u8> = None;
    while let Some(byte) = bytes.get(at) {
        match (quote, *byte) {
            (None, b'>') => return Some(at),
            (None, b'"' | b'\'') => quote = Some(*byte),
            (Some(open), b) if b == open => quote = None,
            _ => {}
        }
        at += 1;
    }
    None
}

fn find_ignore_case(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = haystack.as_bytes().get(from..)?;
    let needle = needle.as_bytes();
    hay.windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|at| from + at)
}

/// What the tokenizer found: the anchors and the document's `<base href>`.
struct Scan {
    anchors: Vec<Anchor>,
    base: Option<String>,
}

fn scan(text: &str, limit: usize) -> Result<Scan, ParseError> {
    let bytes = text.as_bytes();
    let mut anchors = Vec::new();
    let mut base = None;
    let mut at = 0usize;
    while let Some(rel) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|b| *b == b'<'))
    {
        let open = at + rel;
        let rest = text.get(open..).unwrap_or_default();
        if rest.starts_with("<!--") {
            at = text
                .get(open + 4..)
                .and_then(|r| r.find("-->"))
                .map_or(text.len(), |end| open + 4 + end + 3);
            continue;
        }
        let name_end = rest
            .bytes()
            .skip(1)
            .position(|b| b.is_ascii_whitespace() || b == b'>' || b == b'/')
            .map_or(rest.len(), |p| p + 1);
        let tag_name = rest.get(1..name_end).unwrap_or_default();
        let is_anchor = tag_name.eq_ignore_ascii_case("a");
        let is_base = tag_name.eq_ignore_ascii_case("base");
        if !is_anchor && !is_base {
            at = open + 1;
            continue;
        }
        let Some(close) = tag_end(text, open + name_end) else {
            break;
        };
        let attributes = parse_attributes(text.get(open + name_end..close).unwrap_or_default());
        if is_base {
            if base.is_none() {
                base = attributes
                    .iter()
                    .find(|(key, _)| key == "href")
                    .map(|(_, value)| value.clone());
            }
            at = close + 1;
            continue;
        }
        let text_end = find_ignore_case(text, "</a", close + 1).unwrap_or(text.len());
        let label = decode_entities(text.get(close + 1..text_end).unwrap_or_default().trim());
        anchors.push(Anchor {
            attributes,
            text: label,
        });
        if anchors.len() > limit {
            return Err(ParseError::TooMany);
        }
        at = text_end;
    }
    Ok(Scan { anchors, base })
}

fn utf8(bytes: &[u8]) -> Result<&str, ParseError> {
    std::str::from_utf8(bytes).map_err(|_| ParseError::Malformed)
}

/// The URL of an anchor and its hash fragment. The URL is made absolute against `base`; without
/// a base it must already be an absolute `http` or `https` URL, so that a document without a
/// known origin cannot smuggle in a `javascript:` or relative target.
fn resolve(href: &str, base: Option<&Url>) -> Option<(String, Option<(String, String)>)> {
    let (location, fragment) = match href.split_once('#') {
        Some((location, fragment)) => (location, Some(fragment)),
        None => (href, None),
    };
    let url = match base {
        Some(base) => base.join(location).ok()?,
        None => Url::parse(location).ok()?,
    };
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let hash = fragment.and_then(|fragment| {
        let (algo, hex) = fragment.split_once('=')?;
        let hex = hex.to_ascii_lowercase();
        (is_hex(&hex) && !algo.is_empty() && algo.bytes().all(|b| b.is_ascii_alphanumeric()))
            .then(|| (algo.to_ascii_lowercase(), hex))
    });
    Some((url.to_string(), hash))
}

fn core_metadata_of(value: &str) -> CoreMetadataRef {
    match value.split_once('=') {
        Some((algo, hex)) if is_hex(hex) && !algo.is_empty() => CoreMetadataRef::Hashes(
            [(algo.to_ascii_lowercase(), hex.to_ascii_lowercase())]
                .into_iter()
                .collect(),
        ),
        _ => CoreMetadataRef::Available,
    }
}

fn file_name_of(anchor: &Anchor, url: &str) -> Option<String> {
    if !anchor.text.is_empty() {
        return Some(anchor.text.clone());
    }
    let parsed = Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    percent_decode(last).filter(|name| !name.is_empty())
}

/// Decodes `%XX` escapes (and nothing else: `+` stays a plus, it is part of local versions).
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while let Some(byte) = bytes.get(at) {
        if *byte == b'%' {
            let pair = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            at += 3;
        } else {
            out.push(*byte);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Parses a project page. `base` is the URL of the document, against which relative links are
/// resolved (a `<base href>` in the document is applied on top of it). `name` is the normalized
/// name the request was for.
///
/// # Errors
/// [`ParseError::Malformed`] for bytes that are not UTF-8, [`ParseError::TooMany`] for more
/// anchors than [`MAX_FILES_PER_PAGE`].
pub fn parse_project(
    bytes: &[u8],
    base: Option<&Url>,
    name: &str,
) -> Result<ProjectPage, ParseError> {
    let text = utf8(bytes)?;
    let scanned = scan(text, MAX_FILES_PER_PAGE)?;
    let effective_base = match (&scanned.base, base) {
        (Some(declared), Some(page_url)) => page_url.join(declared).ok(),
        (Some(declared), None) => Url::parse(declared).ok(),
        (None, page_url) => page_url.cloned(),
    };
    let mut files = Vec::new();
    for anchor in &scanned.anchors {
        let Some(href) = anchor.attribute("href") else {
            continue;
        };
        let Some((url, hash)) = resolve(href, effective_base.as_ref()) else {
            continue;
        };
        let Some(filename) = file_name_of(anchor, &url) else {
            continue;
        };
        let mut entry = FileEntry::new(filename, url);
        if let Some((algo, hex)) = hash {
            entry.hashes.insert(algo, hex);
        }
        entry.requires_python = anchor
            .attribute("data-requires-python")
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        entry.yanked = anchor.attribute("data-yanked").map(str::to_owned);
        entry.core_metadata = anchor
            .attribute("data-core-metadata")
            .or_else(|| anchor.attribute("data-dist-info-metadata"))
            .filter(|value| !value.eq_ignore_ascii_case("false"))
            .map(core_metadata_of);
        files.push(entry);
    }
    Ok(ProjectPage {
        name: name.to_owned(),
        files,
    })
}

/// Parses a root index. Names that are not valid project names are dropped, the others are
/// normalized.
///
/// # Errors
/// [`ParseError::Malformed`] or [`ParseError::TooMany`].
pub fn parse_root(bytes: &[u8]) -> Result<RootPage, ParseError> {
    let text = utf8(bytes)?;
    let scanned = scan(text, MAX_PROJECTS_PER_INDEX)?;
    let names = scanned
        .anchors
        .iter()
        .filter_map(|anchor| ProjectName::parse(&anchor.text).ok())
        .map(|name| name.as_str().to_owned())
        .collect();
    Ok(RootPage::from_names(names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn page_url() -> Url {
        Url::parse("https://up.test/simple/foo/").unwrap()
    }

    #[test]
    fn renders_pep_503_with_the_attributes() {
        let mut wheel = FileEntry::new(
            "foo-1.0-py3-none-any.whl",
            "https://r.test/packages/foo/foo-1.0-py3-none-any.whl",
        );
        wheel.hashes.insert("sha256".into(), "ab".repeat(32));
        wheel.requires_python = Some(">=3.8,<4".into());
        wheel.core_metadata = Some(CoreMetadataRef::Hashes(
            [("sha256".to_owned(), "cd".repeat(32))]
                .into_iter()
                .collect(),
        ));
        let mut sdist = FileEntry::new("foo-1.0.tar.gz", "https://r.test/p/foo-1.0.tar.gz");
        sdist.yanked = Some("a \"bad\" <build> & more".into());
        let page = ProjectPage {
            name: "foo".into(),
            files: vec![wheel, sdist],
        };
        let html = String::from_utf8(render_project(&page)).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>Links for foo</title>"));
        assert!(html.contains(&format!(
            "href=\"https://r.test/packages/foo/foo-1.0-py3-none-any.whl#sha256={}\"",
            "ab".repeat(32)
        )));
        assert!(html.contains("data-requires-python=\"&gt;=3.8,&lt;4\""));
        assert!(html.contains(&format!(
            "data-core-metadata=\"sha256={}\"",
            "cd".repeat(32)
        )));
        assert!(html.contains(&format!(
            "data-dist-info-metadata=\"sha256={}\"",
            "cd".repeat(32)
        )));
        assert!(html.contains("data-yanked=\"a &quot;bad&quot; &lt;build&gt; &amp; more\""));
        assert!(html.contains(">foo-1.0.tar.gz</a><br/>"));
    }

    #[test]
    fn a_rendered_page_parses_back_to_itself() {
        let mut wheel = FileEntry::new(
            "foo-1.0-py3-none-any.whl",
            "https://r.test/packages/foo/foo-1.0-py3-none-any.whl",
        );
        wheel.hashes.insert("sha256".into(), "ab".repeat(32));
        wheel.requires_python = Some(">=3.8,<4".into());
        wheel.core_metadata = Some(CoreMetadataRef::Available);
        let mut sdist = FileEntry::new("foo-1.0.tar.gz", "https://r.test/p/foo-1.0.tar.gz?x=1&y=2");
        sdist.yanked = Some(String::new());
        sdist.hashes.insert("md5".into(), "00".repeat(16));
        let page = ProjectPage {
            name: "foo".into(),
            files: vec![wheel, sdist],
        };
        let parsed = parse_project(&render_project(&page), None, "foo").unwrap();
        assert_eq!(parsed, page);
    }

    #[test]
    fn reads_what_real_indexes_serve() {
        let html = r#"<!DOCTYPE html><html><head><meta name="pypi:repository-version" content="1.0"><base href="https://files.test/base/"></head>
<body><h1>Links for foo</h1>
<!-- <a href="https://evil.test/x">commented</a> -->
<A HREF='../packages/foo-1.0.tar.gz#SHA256=ABCDEF' data-requires-python="&gt;=3.7" data-yanked>foo-1.0.tar.gz</A><br/>
<a href=foo-1.1-py3-none-any.whl#sha256=00ff data-dist-info-metadata="sha256=aa11" DATA-YANKED="no &amp; reason">
  foo-1.1-py3-none-any.whl
</a>
<a href="javascript:alert(1)">foo-9.tar.gz</a>
<a>no href</a>
<a href="https://other.test/dir/foo-2.0.tar.gz"></a>
<a href="https://other.test/x" data-core-metadata="true">foo-3.0.tar.gz</a>
</body></html>"#;
        let page = parse_project(html.as_bytes(), Some(&page_url()), "foo").unwrap();
        let names: Vec<_> = page.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(
            names,
            [
                "foo-1.0.tar.gz",
                "foo-1.1-py3-none-any.whl",
                "foo-2.0.tar.gz",
                "foo-3.0.tar.gz"
            ]
        );
        let first = &page.files[0];
        assert_eq!(first.url, "https://files.test/packages/foo-1.0.tar.gz");
        assert_eq!(
            first.hashes.get("sha256").map(String::as_str),
            Some("abcdef")
        );
        assert_eq!(first.requires_python.as_deref(), Some(">=3.7"));
        assert_eq!(first.yanked.as_deref(), Some(""));
        let second = &page.files[1];
        assert_eq!(
            second.url,
            "https://files.test/base/foo-1.1-py3-none-any.whl"
        );
        assert_eq!(second.yanked.as_deref(), Some("no & reason"));
        assert!(
            matches!(&second.core_metadata, Some(CoreMetadataRef::Hashes(h)) if h.get("sha256").map(String::as_str) == Some("aa11"))
        );
        assert_eq!(
            page.files[3].core_metadata,
            Some(CoreMetadataRef::Available)
        );
        assert_eq!(
            page.files[2].filename, "foo-2.0.tar.gz",
            "an empty anchor falls back to the URL"
        );
    }

    #[test]
    fn must_not_follow_a_relative_or_non_http_link_without_a_base() {
        let html = r#"<a href="foo-1.0.tar.gz">foo-1.0.tar.gz</a><a href="file:///etc/passwd">foo-2.0.tar.gz</a><a href="ftp://h/foo-3.tar.gz">foo-3.tar.gz</a><a href="https://h/foo-4.tar.gz">foo-4.tar.gz</a>"#;
        let page = parse_project(html.as_bytes(), None, "foo").unwrap();
        assert_eq!(page.files.len(), 1);
        assert_eq!(page.files[0].filename, "foo-4.tar.gz");
    }

    #[test]
    fn the_root_round_trips_and_normalizes() {
        let root = RootPage::from_names(vec!["foo-bar".into(), "baz".into()]);
        let html = render_root(&root);
        assert!(String::from_utf8_lossy(&html).contains("<a href=\"baz/\">baz</a>"));
        assert_eq!(parse_root(&html).unwrap(), root);
        let messy =
            b"<a href='/simple/Foo_Bar/'>Foo_Bar</a><a href='x'>-bad-</a><a href='y'>baz</a>";
        assert_eq!(parse_root(messy).unwrap().projects, ["baz", "foo-bar"]);
    }

    #[test]
    fn the_file_name_fallback_decodes_percent_escapes_only() {
        let html = r#"<a href="https://h/d/foo-1.0%2Bcpu.tar.gz"></a><a href="https://h/d/foo-2.0+x.tar.gz"></a><a href="https://h/d/%zz"></a>"#;
        let page = parse_project(html.as_bytes(), None, "foo").unwrap();
        let names: Vec<_> = page.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["foo-1.0+cpu.tar.gz", "foo-2.0+x.tar.gz"]);
    }

    #[test]
    fn entity_decoding() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;"),
            "a & b <c> \"d\" 'e'"
        );
        assert_eq!(decode_entities("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(
            decode_entities("&unknown; &amp &#xZZ; &#1114112; tail&"),
            "&unknown; &amp &#xZZ; &#1114112; tail&"
        );
        assert_eq!(decode_entities("plain"), "plain");
    }

    #[test]
    fn refuses_what_is_not_text_and_too_many_anchors() {
        assert_eq!(
            parse_project(&[0xff, 0xfe], None, "p"),
            Err(ParseError::Malformed)
        );
        assert_eq!(parse_root(&[0xff]), Err(ParseError::Malformed));
        let many = "<a href=\"https://h/x\">a-1.tar.gz</a>".repeat(MAX_FILES_PER_PAGE + 1);
        assert_eq!(
            parse_project(many.as_bytes(), None, "p"),
            Err(ParseError::TooMany)
        );
    }

    #[test]
    fn unterminated_constructs_do_not_hang_or_panic() {
        for html in [
            "<a href=\"x",
            "<a",
            "<a href=https://h/x",
            "<!-- <a href=\"https://h/x\">",
            "<a href='https://h/f-1.tar.gz'>f-1.tar.gz",
            "<base href=\"",
            "<<<<<a>>>",
        ] {
            let _ = parse_project(html.as_bytes(), None, "p");
        }
    }

    proptest! {
        #[test]
        fn parse_never_panics(text in "\\PC{0,300}") {
            let _ = parse_project(text.as_bytes(), Some(&page_url()), "p");
            let _ = parse_root(text.as_bytes());
        }

        #[test]
        fn parse_never_panics_on_tag_soup(text in "(<a( [a-z-]{1,12}(=(\"[ -~]{0,12}\"|'[ -~]{0,12}'|[a-z0-9]{0,6}))?){0,4}>|</a>|<base href=\"[ -~]{0,10}\">|<!--|-->|&[a-z#0-9]{0,6};?|[ -~]{0,6}){0,30}") {
            let _ = parse_project(text.as_bytes(), Some(&page_url()), "p");
        }

        #[test]
        fn generated_pages_round_trip(
            files in proptest::collection::vec(("[a-z]{1,6}", 0u32..20, any::<bool>(), proptest::option::of("[ -~]{0,12}"), proptest::option::of("[ -~]{1,10}")), 0..8)
        ) {
            let mut page = ProjectPage { name: "p".into(), files: Vec::new() };
            for (stem, minor, yanked, reason, requires) in files {
                let mut entry = FileEntry::new(format!("{stem}-1.{minor}.tar.gz"), format!("https://h/{stem}/{minor}"));
                entry.hashes.insert("sha256".into(), "ab".repeat(32));
                if yanked {
                    entry.yanked = Some(reason.unwrap_or_default());
                }
                entry.requires_python = requires.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
                page.files.push(entry);
            }
            let parsed = parse_project(&render_project(&page), None, "p").unwrap();
            prop_assert_eq!(parsed, page);
        }
    }
}
