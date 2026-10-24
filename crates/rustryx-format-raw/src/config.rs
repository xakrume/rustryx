//! The `format_config` of a raw repository.
//!
//! ```json
//! { "content_disposition": "attachment" }
//! ```
//!
//! `content_disposition` is `attachment` (the default) or `inline`. It only matters for content
//! a browser would execute or render with scripts (html, svg, xml, javascript): `attachment`
//! makes the browser save such a file instead of rendering it in the origin of the repository,
//! which is what stops a stored upload from becoming stored XSS. `inline` is a conscious opt
//! in for repositories that host static pages; the response still carries `nosniff` and a
//! sandboxing `Content-Security-Policy` from the shared pipeline, so even then a script in the
//! file cannot run with the origin of the artifact manager.
//!
//! An unknown or malformed value means `attachment`: the safe reading wins when the setting is
//! unclear.

use rustryx_core::model::Repository;

/// The key of the setting in `Repository::format_config`.
pub const CONTENT_DISPOSITION_KEY: &str = "content_disposition";

/// How content that a browser could execute is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentDisposition {
    /// `Content-Disposition: attachment`: the browser downloads the file.
    #[default]
    Attachment,
    /// No disposition header: the browser may render the file (sandboxed by the CSP).
    Inline,
}

/// Settings of one raw repository, read from its `format_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawConfig {
    /// Delivery of active content.
    pub content_disposition: ContentDisposition,
}

impl RawConfig {
    /// Reads the settings of `repository`. Never fails: unknown values fall back to the safe
    /// default.
    #[must_use]
    pub fn of(repository: &Repository) -> Self {
        Self::from_value(&repository.format_config)
    }

    /// Reads the settings from a `format_config` value.
    #[must_use]
    pub fn from_value(value: &serde_json::Value) -> Self {
        let inline = value
            .get(CONTENT_DISPOSITION_KEY)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("inline"));
        Self {
            content_disposition: if inline {
                ContentDisposition::Inline
            } else {
                ContentDisposition::Attachment
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_default_is_attachment() {
        assert_eq!(
            RawConfig::from_value(&json!({})).content_disposition,
            ContentDisposition::Attachment
        );
        assert_eq!(
            RawConfig::default().content_disposition,
            ContentDisposition::Attachment
        );
    }

    #[test]
    fn inline_is_an_explicit_opt_in_in_any_case() {
        for value in ["inline", "INLINE", " Inline "] {
            let config = RawConfig::from_value(&json!({ "content_disposition": value }));
            assert_eq!(
                config.content_disposition,
                ContentDisposition::Inline,
                "{value}"
            );
        }
    }

    #[test]
    fn unknown_or_malformed_values_mean_attachment() {
        for value in [
            json!({ "content_disposition": "sometimes" }),
            json!({ "content_disposition": 1 }),
            json!({ "content_disposition": null }),
            json!({ "content_disposition": ["inline"] }),
            json!("inline"),
            json!(null),
            json!([]),
        ] {
            assert_eq!(
                RawConfig::from_value(&value).content_disposition,
                ContentDisposition::Attachment,
                "{value}"
            );
        }
    }
}
