//! Secret scrubbing for log output.
//!
//! Two independent defences, because either alone has gaps:
//!
//! 1. A field whose *name* suggests a secret (`password`, `authorization`,
//!    `client_secret`, ...) is replaced wholesale, whatever its value looks like.
//! 2. Every text value, including the message, is scanned for secret *shapes*:
//!    `Bearer`/`Basic` credentials, `ryx_` API tokens, JWTs, PEM private keys,
//!    URL user-info and `key=value` pairs of sensitive keys. This catches a secret
//!    that was formatted into a message or a URL.
//!
//! The scan prefers false positives (a prose "Basic authentication" becomes
//! `Basic [REDACTED]`) to false negatives: a log line that lost a word is cheaper
//! than one that leaked a credential.

use std::borrow::Cow;

use regex::{Regex, RegexSet};

/// Text that replaces a redacted value.
pub const REDACTED: &str = "[REDACTED]";

/// Longest text value kept in one log field, in characters. Attacker-controlled
/// data (paths, user agents) must not be able to flood the log pipeline.
pub const MAX_FIELD_CHARS: usize = 2048;

/// Substrings of field names that mark the value as secret (after lowercasing and
/// mapping `-` to `_`).
const SENSITIVE_NAME_NEEDLES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "private_key",
    "credential",
    "session",
    "signature",
    "dsn",
    "database_url",
];

/// Names that contain a needle but are identifiers or metadata, not secrets.
const ALLOWED_NAMES: &[&str] = &[
    "token_id",
    "token_scopes",
    "token_type",
    "tokens_valid_after",
];

/// (pattern, replacement). Replacements use `${n}` for captured prefixes.
const RULES: &[(&str, &str)] = &[
    (
        r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}",
        "${1} [REDACTED]",
    ),
    (r"\bryx_[A-Za-z0-9_\-]{8,}", "ryx_[REDACTED]"),
    (
        r#"(?i)((?:^|[?&;\s])(?:access_token|id_token|refresh_token|token|password|passwd|secret|client_secret|api_key|apikey|code|sig|signature|x-amz-signature|x-amz-credential)=)[^&\s"']+"#,
        "${1}[REDACTED]",
    ),
    (
        r"(?i)\b([a-z][a-z0-9+.\-]*://[^/\s:@]+:)[^@/\s]+@",
        "${1}[REDACTED]@",
    ),
    (
        r"\beyJ[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]*",
        "[REDACTED_JWT]",
    ),
    (
        r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----(?:.*?-----END [A-Z ]*PRIVATE KEY-----|.*)",
        "[REDACTED_PRIVATE_KEY]",
    ),
    (
        r#"(?i)\b(password|passwd|secret|token|api[_\-]?key|authorization)\s*[=:]\s*[^\s,;"']+"#,
        "${1}=[REDACTED]",
    ),
];

/// Scrubs field values and recognises sensitive field names.
#[derive(Debug, Clone)]
pub struct Redactor {
    /// Fast pre-check: most values match nothing, and one automaton pass decides.
    any: RegexSet,
    rules: Vec<(Regex, &'static str)>,
}

impl Redactor {
    /// Builds the standard redactor.
    ///
    /// # Errors
    /// A pattern failed to compile. The patterns are constants, so this signals a
    /// programming error that a unit test catches; it is a `Result` only because
    /// `unwrap`/`expect` are not allowed outside tests.
    pub fn standard() -> Result<Self, regex::Error> {
        let any = RegexSet::new(RULES.iter().map(|(pattern, _)| *pattern))?;
        let rules = RULES
            .iter()
            .map(|(pattern, replacement)| Regex::new(pattern).map(|re| (re, *replacement)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { any, rules })
    }

    /// True when a field with this name must have its value replaced entirely.
    #[must_use]
    pub fn is_sensitive_name(name: &str) -> bool {
        let normalized = name.to_ascii_lowercase().replace('-', "_");
        if ALLOWED_NAMES.contains(&normalized.as_str()) {
            return false;
        }
        SENSITIVE_NAME_NEEDLES
            .iter()
            .any(|needle| normalized.contains(needle))
    }

    /// Replaces secret-shaped substrings and caps the length.
    #[must_use]
    pub fn scrub<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let capped = truncate_chars(text);
        if !self.any.is_match(&capped) {
            return capped;
        }
        let mut current = capped.into_owned();
        for (regex, replacement) in &self.rules {
            if let Cow::Owned(replaced) = regex.replace_all(&current, *replacement) {
                current = replaced;
            }
        }
        Cow::Owned(current)
    }
}

fn truncate_chars(text: &str) -> Cow<'_, str> {
    match text.char_indices().nth(MAX_FIELD_CHARS) {
        None => Cow::Borrowed(text),
        Some((byte_index, _)) => {
            let mut cut = text.get(..byte_index).unwrap_or_default().to_owned();
            cut.push_str("...[truncated]");
            Cow::Owned(cut)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::standard().expect("patterns compile")
    }

    #[test]
    fn patterns_compile() {
        assert!(Redactor::standard().is_ok());
    }

    #[test]
    fn sensitive_names_are_recognised_case_and_dash_insensitively() {
        for name in [
            "password",
            "Authorization",
            "client_secret",
            "x-api-key",
            "Set-Cookie",
            "access_token",
            "database_url",
            "bind_password",
            "SessionId",
        ] {
            assert!(Redactor::is_sensitive_name(name), "{name}");
        }
        for name in [
            "user",
            "repo",
            "status",
            "token_id",
            "token_scopes",
            "route",
            "request_id",
        ] {
            assert!(!Redactor::is_sensitive_name(name), "{name}");
        }
    }

    #[test]
    fn must_not_log_bearer_basic_and_api_tokens() {
        let r = redactor();
        for (input, leaked) in [
            ("Authorization: Bearer abcdef0123456789", "abcdef0123456789"),
            ("header Basic dXNlcjpwYXNzd29yZA==", "dXNlcjpwYXNzd29yZA"),
            ("token ryx_AbCdEf0123456789 rejected", "AbCdEf0123456789"),
        ] {
            let out = r.scrub(input);
            assert!(!out.contains(leaked), "{input} -> {out}");
            assert!(out.contains("[REDACTED"), "{input} -> {out}");
        }
    }

    #[test]
    fn must_not_log_jwts_and_private_keys() {
        let r = redactor();
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.c2lnbmF0dXJl";
        let message = format!("got {jwt} from client");
        let out = r.scrub(&message);
        assert!(!out.contains("eyJhbGci"));
        assert!(out.contains("[REDACTED_JWT]"));

        let pem = "key -----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBg\n-----END PRIVATE KEY----- end";
        let out = r.scrub(pem);
        assert!(!out.contains("MIIEvQ"));
        assert!(out.contains("[REDACTED_PRIVATE_KEY]"));

        let truncated = r.scrub("-----BEGIN RSA PRIVATE KEY-----\nMIIE");
        assert!(!truncated.contains("MIIE"));
    }

    #[test]
    fn must_not_log_url_credentials_or_secret_query_parameters() {
        let r = redactor();
        let out = r.scrub("connect postgres://app:hunter2@db:5432/x failed");
        assert!(!out.contains("hunter2"));
        assert!(out.contains("postgres://app:[REDACTED]@db"));

        let out = r.scrub("GET /v2/token?service=x&access_token=abc123xyz&scope=pull");
        assert!(!out.contains("abc123xyz"));
        assert!(
            out.contains("scope=pull"),
            "other parameters survive: {out}"
        );

        let out = r.scrub("callback?code=SPLX9&state=ok");
        assert!(!out.contains("SPLX9"));
    }

    #[test]
    fn must_not_log_key_value_secrets_in_free_text() {
        let r = redactor();
        for input in [
            "login failed password=hunter2 user=bob",
            "using secret: s3cr3t-value now",
            "api_key=abcd1234",
            "token=zzz999",
        ] {
            let out = r.scrub(input);
            for leaked in ["hunter2", "s3cr3t-value", "abcd1234", "zzz999"] {
                assert!(!out.contains(leaked), "{input} -> {out}");
            }
        }
    }

    #[test]
    fn harmless_text_is_untouched_and_borrowed() {
        let r = redactor();
        let text = "uploaded maven-central/org/example/lib/1.0/lib-1.0.jar in 12 ms";
        let out = r.scrub(text);
        assert_eq!(out, text);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn must_not_let_attacker_controlled_values_flood_the_log() {
        let r = redactor();
        let long = "a".repeat(MAX_FIELD_CHARS * 3);
        let out = r.scrub(&long);
        assert!(out.chars().count() < MAX_FIELD_CHARS + 32);
        assert!(out.ends_with("[truncated]"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let r = redactor();
        let text = "é".repeat(MAX_FIELD_CHARS + 10);
        let out = r.scrub(&text);
        assert!(out.starts_with('é') && out.ends_with("[truncated]"));
    }
}
