//! Content negotiation of the simple API (PEP 691).
//!
//! Specification: <https://packaging.python.org/en/latest/specifications/simple-repository-api/>.
//! Supported media types: `application/vnd.pypi.simple.v1+json`, `application/vnd.pypi.simple.v1+html`,
//! their `latest` aliases, and `text/html`, the PEP 503 compatible spelling of the HTML form.
//! A request without `Accept`, or with `*/*`, gets HTML: every PEP 503 client can read it.
//! An `Accept` that lists only types this repository does not produce is answered 406, which
//! the PEP recommends over 300.

/// The representation chosen for a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Representation {
    /// `application/vnd.pypi.simple.v1+json`.
    Json,
    /// `application/vnd.pypi.simple.v1+html`.
    HtmlV1,
    /// `text/html`, the PEP 503 spelling.
    TextHtml,
}

impl Representation {
    /// The `Content-Type` of the response. A concrete version is always named, never `latest`.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/vnd.pypi.simple.v1+json",
            Self::HtmlV1 => "application/vnd.pypi.simple.v1+html",
            Self::TextHtml => "text/html; charset=utf-8",
        }
    }

    /// True for the JSON representation.
    #[must_use]
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

/// How specifically a media range names a representation; a more specific range wins a tie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Specificity {
    Any,
    Type,
    Exact,
}

/// Picks the representation for an `Accept` header value, or `None` when the client accepts
/// none of them.
#[must_use]
pub fn negotiate(accept: Option<&str>) -> Option<Representation> {
    // (quality in thousandths, specificity, position) decide; later entries lose ties.
    type Score = (u16, Specificity, std::cmp::Reverse<usize>);
    let Some(accept) = accept.map(str::trim).filter(|a| !a.is_empty()) else {
        return Some(Representation::TextHtml);
    };
    let mut best: Option<(Score, Representation)> = None;
    for (position, range) in accept.split(',').enumerate() {
        let mut pieces = range.split(';');
        let media = pieces
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let quality = pieces
            .filter_map(|param| {
                let (key, value) = param.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| parse_quality(value.trim()))
            })
            .next_back()
            .unwrap_or(1000);
        if quality == 0 {
            continue;
        }
        let (specificity, produced): (Specificity, &[Representation]) = match media.as_str() {
            "application/vnd.pypi.simple.v1+json" | "application/vnd.pypi.simple.latest+json" => {
                (Specificity::Exact, &[Representation::Json])
            }
            "application/vnd.pypi.simple.v1+html" | "application/vnd.pypi.simple.latest+html" => {
                (Specificity::Exact, &[Representation::HtmlV1])
            }
            "text/html" => (Specificity::Exact, &[Representation::TextHtml]),
            "application/*" => (
                Specificity::Type,
                &[Representation::Json, Representation::HtmlV1],
            ),
            "text/*" => (Specificity::Type, &[Representation::TextHtml]),
            "*/*" | "*" => (Specificity::Any, &[Representation::TextHtml]),
            _ => continue,
        };
        for representation in produced {
            let key = (quality, specificity, std::cmp::Reverse(position));
            if best.as_ref().is_none_or(|(current, _)| key > *current) {
                best = Some((key, *representation));
            }
        }
    }
    best.map(|(_, representation)| representation)
}

/// A quality value in thousandths; garbage counts as the default, which is what a lenient
/// server does with a header it cannot read.
fn parse_quality(text: &str) -> u16 {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole != "0" || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return 1000;
    }
    // At most three decimals count; the digits are ASCII, so slicing by bytes is safe.
    let digits = fraction.get(..fraction.len().min(3)).unwrap_or_default();
    let value = digits.parse::<u16>().unwrap_or(0);
    let missing = u32::try_from(3 - digits.len()).unwrap_or(0);
    value.saturating_mul(10u16.pow(missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIP: &str = "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html; q=0.1, text/html; q=0.01";

    #[test]
    fn the_accept_headers_of_real_clients() {
        assert_eq!(negotiate(Some(PIP)), Some(Representation::Json));
        // pip before 22.3 and curl.
        assert_eq!(negotiate(Some("text/html")), Some(Representation::TextHtml));
        assert_eq!(negotiate(Some("*/*")), Some(Representation::TextHtml));
        assert_eq!(negotiate(None), Some(Representation::TextHtml));
        assert_eq!(negotiate(Some("  ")), Some(Representation::TextHtml));
        // uv
        assert_eq!(
            negotiate(Some(
                "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.01"
            )),
            Some(Representation::Json)
        );
    }

    #[test]
    fn quality_values_decide() {
        assert_eq!(
            negotiate(Some(
                "application/vnd.pypi.simple.v1+json;q=0.5, text/html;q=0.9"
            )),
            Some(Representation::TextHtml)
        );
        assert_eq!(
            negotiate(Some(
                "application/vnd.pypi.simple.v1+html;q=0.8, application/vnd.pypi.simple.latest+json;q=0.8"
            )),
            Some(Representation::HtmlV1),
            "equal quality: the one listed first"
        );
        assert_eq!(
            negotiate(Some(
                "application/vnd.pypi.simple.v1+json;q=0, text/html;q=0.001"
            )),
            Some(Representation::TextHtml)
        );
        assert_eq!(
            negotiate(Some("application/vnd.pypi.simple.v1+json;q=0")),
            None
        );
    }

    #[test]
    fn latest_and_wildcards() {
        assert_eq!(
            negotiate(Some("application/vnd.pypi.simple.latest+json")),
            Some(Representation::Json)
        );
        assert_eq!(
            negotiate(Some("application/vnd.pypi.simple.latest+html")),
            Some(Representation::HtmlV1)
        );
        assert_eq!(negotiate(Some("application/*")), Some(Representation::Json));
        assert_eq!(negotiate(Some("text/*")), Some(Representation::TextHtml));
        assert_eq!(
            negotiate(Some("*/*;q=0.8, application/vnd.pypi.simple.v1+json;q=0.8")),
            Some(Representation::Json),
            "a named type beats a wildcard at equal quality"
        );
    }

    #[test]
    fn unsupported_types_are_not_acceptable() {
        assert_eq!(negotiate(Some("application/json")), None);
        assert_eq!(negotiate(Some("image/png, application/xml")), None);
        assert_eq!(negotiate(Some("application/vnd.pypi.simple.v2+json")), None);
        assert_eq!(negotiate(Some(";;;,,,")), None);
    }

    #[test]
    fn garbage_quality_counts_as_one() {
        assert_eq!(parse_quality("abc"), 1000);
        assert_eq!(parse_quality("1"), 1000);
        assert_eq!(parse_quality("0.5"), 500);
        assert_eq!(parse_quality("0.123456"), 123);
        assert_eq!(parse_quality("0.x"), 1000);
        assert_eq!(parse_quality("0"), 0);
        assert_eq!(parse_quality("0.05"), 50);
    }

    #[test]
    fn content_types_name_a_concrete_version() {
        assert_eq!(
            Representation::Json.content_type(),
            "application/vnd.pypi.simple.v1+json"
        );
        assert_eq!(
            Representation::HtmlV1.content_type(),
            "application/vnd.pypi.simple.v1+html"
        );
        assert!(
            Representation::TextHtml
                .content_type()
                .starts_with("text/html")
        );
        assert!(Representation::Json.is_json());
        assert!(!Representation::HtmlV1.is_json());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(text in "\\PC{0,200}") {
            let _ = negotiate(Some(&text));
        }
    }
}
