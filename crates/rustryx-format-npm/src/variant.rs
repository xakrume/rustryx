//! Content negotiation of the package document: full or abbreviated.
//!
//! The abbreviated form (`application/vnd.npm.install-v1+json`) carries only what an install needs
//! and is what `npm install` asks for; the full form is what `npm view` and `npm deprecate` read.
//! Specification: <https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md>.

use http::{HeaderMap, header};

use crate::packument::{ABBREVIATED_MEDIA_TYPE, FULL_MEDIA_TYPE};

/// Which form of the package document a client asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// `application/json`.
    Full,
    /// `application/vnd.npm.install-v1+json`.
    Abbreviated,
}

impl Variant {
    /// The variant for an `Accept` header: abbreviated when the install media type is acceptable
    /// (`q` above zero) and at least as preferred as `application/json`. No header, `*/*` and
    /// plain `application/json` give the full document.
    #[must_use]
    pub fn of_accept(accept: &str) -> Self {
        let mut abbreviated: Option<f32> = None;
        let mut json: Option<f32> = None;
        for range in accept.split(',') {
            let mut parts = range.split(';');
            let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let quality = parts
                .filter_map(|p| p.trim().strip_prefix("q="))
                .find_map(|q| q.trim().parse::<f32>().ok())
                .map_or(1.0, |q| q.clamp(0.0, 1.0));
            if media == ABBREVIATED_MEDIA_TYPE {
                abbreviated = Some(quality);
            } else if media == FULL_MEDIA_TYPE {
                json = Some(quality);
            }
        }
        match abbreviated {
            Some(q) if q > 0.0 && json.is_none_or(|j| q >= j) => Self::Abbreviated,
            _ => Self::Full,
        }
    }

    /// The variant for the request headers.
    #[must_use]
    pub fn of_headers(headers: &HeaderMap) -> Self {
        headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map_or(Self::Full, Self::of_accept)
    }

    /// Media type of the response.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Full => FULL_MEDIA_TYPE,
            Self::Abbreviated => ABBREVIATED_MEDIA_TYPE,
        }
    }

    /// Metric label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Abbreviated => "abbreviated",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_install_asks_for_the_abbreviated_form() {
        let accept = "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*";
        assert_eq!(Variant::of_accept(accept), Variant::Abbreviated);
    }

    #[test]
    fn everything_else_gets_the_full_document() {
        for accept in ["", "*/*", "application/json", "text/html", "application/*"] {
            assert_eq!(Variant::of_accept(accept), Variant::Full, "{accept:?}");
        }
    }

    #[test]
    fn json_preferred_over_the_install_type_wins() {
        let accept = "application/vnd.npm.install-v1+json; q=0.5, application/json; q=0.9";
        assert_eq!(Variant::of_accept(accept), Variant::Full);
        assert_eq!(
            Variant::of_accept("application/vnd.npm.install-v1+json;q=0"),
            Variant::Full
        );
        assert_eq!(
            Variant::of_accept("application/json, application/vnd.npm.install-v1+json"),
            Variant::Abbreviated,
            "equal preference favours the install type, as the registry does"
        );
    }

    #[test]
    fn garbage_is_not_an_error() {
        for accept in [
            ";;;",
            "application/vnd.npm.install-v1+json;q=abc",
            ",,,",
            "q=1",
        ] {
            let _ = Variant::of_accept(accept);
        }
        assert_eq!(
            Variant::of_accept("APPLICATION/VND.NPM.INSTALL-V1+JSON"),
            Variant::Abbreviated
        );
    }

    #[test]
    fn headers_and_labels() {
        let mut headers = HeaderMap::new();
        assert_eq!(Variant::of_headers(&headers), Variant::Full);
        headers.insert(
            header::ACCEPT,
            "application/vnd.npm.install-v1+json".parse().unwrap(),
        );
        let variant = Variant::of_headers(&headers);
        assert_eq!(variant, Variant::Abbreviated);
        assert_eq!(variant.media_type(), ABBREVIATED_MEDIA_TYPE);
        assert_eq!(variant.label(), "abbreviated");
        assert_eq!(Variant::Full.media_type(), FULL_MEDIA_TYPE);
        assert_eq!(Variant::Full.label(), "full");
    }
}
