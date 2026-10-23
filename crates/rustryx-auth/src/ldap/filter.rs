//! LDAP search filter construction (RFC 4515).
//!
//! The only way user controlled text reaches a filter is through
//! [`FilterTemplate::render`], which escapes every substituted value. There is no
//! other public function that builds a filter from input, so the rule "never
//! concatenate raw input into a filter" is held by the type, not by discipline.

use std::borrow::Cow;

/// A value that can be substituted into a filter template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// `{username}`: the login name typed by the user, or the canonical user name
    /// when a group filter is rendered for a known user.
    Username,
    /// `{dn}`: a distinguished name returned by the directory.
    Dn,
}

impl Placeholder {
    const fn token(self) -> &'static str {
        match self {
            Self::Username => "username",
            Self::Dn => "dn",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "username" => Some(Self::Username),
            "dn" => Some(Self::Dn),
            _ => None,
        }
    }
}

/// Values substituted by [`FilterTemplate::render`]. A value that no placeholder
/// of the template refers to is never read.
#[derive(Debug, Clone, Copy)]
pub struct Substitution<'a> {
    /// Value of `{username}`.
    pub username: &'a str,
    /// Value of `{dn}`.
    pub dn: &'a str,
}

/// Why a filter template was rejected at configuration time.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterTemplateError {
    /// The template is empty or only whitespace.
    #[error("filter template is empty")]
    Empty,
    /// A `{name}` token names no known placeholder.
    #[error("unknown placeholder `{{{0}}}` in filter template")]
    UnknownPlaceholder(String),
    /// A known placeholder is used where the template kind does not provide it.
    #[error("placeholder `{{{0}}}` is not available in this filter template")]
    PlaceholderNotAllowed(&'static str),
    /// None of the required placeholders is used.
    #[error("filter template must use the placeholder {0}")]
    MissingPlaceholder(String),
    /// A brace is not part of a `{name}` token.
    #[error("unbalanced brace in filter template")]
    UnbalancedBrace,
    /// The template is not exactly one syntactically valid filter.
    #[error("filter template is not a single valid LDAP filter")]
    InvalidFilter,
}

/// Escapes a value for use as an assertion value in a filter (RFC 4515 section 3).
///
/// `\`, `*`, `(`, `)` and NUL become `\5c`, `\2a`, `\28`, `\29` and `\00`. Those are
/// the only characters that can change the structure of a filter; every other
/// character, including non-ASCII UTF-8, is a literal there.
#[must_use]
pub fn escape_filter_value(value: &str) -> Cow<'_, str> {
    if !value.chars().any(needs_filter_escape) {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\5c"),
            '*' => escaped.push_str("\\2a"),
            '(' => escaped.push_str("\\28"),
            ')' => escaped.push_str("\\29"),
            '\0' => escaped.push_str("\\00"),
            other => escaped.push(other),
        }
    }
    Cow::Owned(escaped)
}

const fn needs_filter_escape(ch: char) -> bool {
    matches!(ch, '\\' | '*' | '(' | ')' | '\0')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Value(Placeholder),
}

/// A parsed filter template such as `(uid={username})`.
///
/// Parsing happens once, at configuration time, so a typo in the template stops
/// the process from starting instead of failing every login at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterTemplate {
    pieces: Vec<Piece>,
}

impl FilterTemplate {
    /// Parses `template`. `allowed` lists the placeholders the template may use and
    /// at least one of `required` must appear.
    ///
    /// The template, with every placeholder replaced by a probe value, must be
    /// exactly one filter that the `ldap3` filter parser accepts. That parser is the
    /// one that later builds the request, so "valid here" means "encodable there".
    pub fn parse(
        template: &str,
        allowed: &[Placeholder],
        required: &[Placeholder],
    ) -> Result<Self, FilterTemplateError> {
        if template.trim().is_empty() {
            return Err(FilterTemplateError::Empty);
        }
        let pieces = split_pieces(template, allowed)?;
        if !pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Value(p) if required.contains(p)))
        {
            let names: Vec<String> = required
                .iter()
                .map(|p| format!("{{{}}}", p.token()))
                .collect();
            return Err(FilterTemplateError::MissingPlaceholder(names.join(" or ")));
        }
        let parsed = Self { pieces };
        let probe = parsed.render(Substitution {
            username: "probe",
            dn: "probe",
        });
        // The `ldap3` parser tolerates a filter without the outer parentheses; RFC 4515
        // does not, and a template that relies on the leniency would break the day the
        // parser is tightened, so the strict form is required here.
        let parenthesised = probe.starts_with('(') && probe.ends_with(')');
        if !parenthesised || ldap3::parse_filter(&probe).is_err() {
            return Err(FilterTemplateError::InvalidFilter);
        }
        Ok(parsed)
    }

    /// Renders the filter with every substituted value escaped.
    ///
    /// Substitution is a single pass over the parsed pieces, so text inside a value
    /// that looks like `{dn}` stays literal and is never expanded a second time.
    #[must_use]
    pub fn render(&self, values: Substitution<'_>) -> String {
        let mut filter = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Literal(text) => filter.push_str(text),
                Piece::Value(Placeholder::Username) => {
                    filter.push_str(&escape_filter_value(values.username));
                }
                Piece::Value(Placeholder::Dn) => {
                    filter.push_str(&escape_filter_value(values.dn));
                }
            }
        }
        filter
    }
}

fn split_pieces(
    template: &str,
    allowed: &[Placeholder],
) -> Result<Vec<Piece>, FilterTemplateError> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut rest = template;
    while let Some(index) = rest.find(['{', '}']) {
        literal.push_str(&rest[..index]);
        let tail = &rest[index..];
        if !tail.starts_with('{') {
            return Err(FilterTemplateError::UnbalancedBrace);
        }
        let Some(end) = tail.find('}') else {
            return Err(FilterTemplateError::UnbalancedBrace);
        };
        let token = &tail[1..end];
        let placeholder = Placeholder::from_token(token)
            .ok_or_else(|| FilterTemplateError::UnknownPlaceholder(token.to_owned()))?;
        if !allowed.contains(&placeholder) {
            return Err(FilterTemplateError::PlaceholderNotAllowed(
                placeholder.token(),
            ));
        }
        if !literal.is_empty() {
            pieces.push(Piece::Literal(std::mem::take(&mut literal)));
        }
        pieces.push(Piece::Value(placeholder));
        rest = &tail[end + 1..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        pieces.push(Piece::Literal(literal));
    }
    Ok(pieces)
}

/// Decodes `\hh` escapes of an RFC 4515 assertion value. Returns `None` for a raw
/// special character or a malformed escape. Used by tests and by the fake
/// directory to prove that a rendered filter still parses as the intended value.
#[cfg(test)]
pub(crate) fn unescape_filter_value(escaped: &str) -> Option<String> {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                let hex = escaped.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'(' | b')' | b'*' | 0 => return None,
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldap3::asn1::{ASNTag, PL, StructureTag, TagClass};
    use proptest::prelude::*;

    const USER_ONLY: &[Placeholder] = &[Placeholder::Username];
    const BOTH: &[Placeholder] = &[Placeholder::Username, Placeholder::Dn];

    fn user_template(text: &str) -> FilterTemplate {
        FilterTemplate::parse(text, USER_ONLY, USER_ONLY).unwrap()
    }

    /// Returns `(attribute, value)` when `filter` parses as exactly one equality
    /// match, which is what a login must always produce.
    fn equality_match(filter: &str) -> Option<(String, Vec<u8>)> {
        let tag = ldap3::parse_filter(filter).ok()?;
        let structure: StructureTag = tag.into_structure();
        if structure.class != TagClass::Context || structure.id != 3 {
            return None;
        }
        let PL::C(children) = structure.payload else {
            return None;
        };
        let mut parts = children.into_iter();
        let attribute = parts.next()?.expect_primitive()?;
        let value = parts.next()?.expect_primitive()?;
        if parts.next().is_some() {
            return None;
        }
        Some((String::from_utf8(attribute).ok()?, value))
    }

    #[test]
    fn escapes_the_five_rfc4515_characters_in_lowercase_hex() {
        assert_eq!(
            escape_filter_value("a\\b*c(d)e\0f"),
            "a\\5cb\\2ac\\28d\\29e\\00f"
        );
    }

    #[test]
    fn leaves_plain_and_non_ascii_values_untouched() {
        for plain in [
            "alice",
            "",
            "j.doe-1",
            "Zoë Müller",
            "名前",
            "a b",
            "user@example.org",
        ] {
            assert!(
                matches!(escape_filter_value(plain), Cow::Borrowed(_)),
                "{plain}"
            );
        }
    }

    #[test]
    fn must_not_let_injection_strings_change_the_filter_structure() {
        let template = user_template("(uid={username})");
        for attack in [
            "*",
            "*)(uid=*",
            "admin)(|(uid=*",
            "alice)(&(objectClass=*)",
            "\\",
            "\\00",
            "a\0b",
            ")(|(cn=*))(|(cn=",
            "*)(userPassword=*",
        ] {
            let filter = template.render(Substitution {
                username: attack,
                dn: "",
            });
            let parsed = equality_match(&filter);
            assert!(parsed.is_some(), "not one equality match: {filter}");
            let (attribute, value) = parsed.unwrap();
            assert_eq!(attribute, "uid");
            assert_eq!(
                value,
                attack.as_bytes(),
                "value must round trip for {attack:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn must_not_let_special_characters_change_filter_structure(
            chars in prop::collection::vec(
                prop_oneof![
                    any::<char>(),
                    prop::sample::select(vec!['(', ')', '*', '\\', '\0', '{', '}', '=', '&', '|', '!']),
                ],
                0..48,
            )
        ) {
            let value: String = chars.into_iter().collect();
            let template = user_template("(uid={username})");
            let filter = template.render(Substitution { username: &value, dn: "" });
            let parsed = equality_match(&filter);
            prop_assert!(parsed.is_some(), "filter {filter:?} is not one equality match");
            let (attribute, bytes) = parsed.unwrap();
            prop_assert_eq!(attribute, "uid");
            prop_assert_eq!(bytes, value.as_bytes().to_vec());
        }

        #[test]
        fn escaped_values_contain_no_raw_special_characters_and_round_trip(
            chars in prop::collection::vec(any::<char>(), 0..48)
        ) {
            let value: String = chars.into_iter().collect();
            let escaped = escape_filter_value(&value);
            prop_assert_eq!(unescape_filter_value(&escaped), Some(value.clone()));
            let reference = ldap3::ldap_escape(value.as_str()).into_owned();
            prop_assert_eq!(
                escaped.as_ref(),
                reference.as_str(),
                "must agree with the ldap3 reference implementation"
            );
        }
    }

    #[test]
    fn must_not_expand_a_placeholder_found_inside_a_value() {
        let template = FilterTemplate::parse("(&(a={username})(b={dn}))", BOTH, USER_ONLY).unwrap();
        let filter = template.render(Substitution {
            username: "{dn}",
            dn: "x",
        });
        assert_eq!(filter, "(&(a={dn})(b=x))");
    }

    #[test]
    fn dn_values_are_escaped_like_any_other_value() {
        let template = FilterTemplate::parse("(member={dn})", BOTH, &[Placeholder::Dn]).unwrap();
        let filter = template.render(Substitution {
            username: "",
            dn: "CN=Smith\\, John (IT)*,OU=x",
        });
        assert_eq!(filter, "(member=CN=Smith\\5c, John \\28IT\\29\\2a,OU=x)");
        assert!(ldap3::parse_filter(&filter).is_ok());
    }

    #[test]
    fn rejects_templates_that_cannot_be_used() {
        let cases: [(&str, &[Placeholder], &[Placeholder], FilterTemplateError); 8] = [
            ("", USER_ONLY, USER_ONLY, FilterTemplateError::Empty),
            ("   ", USER_ONLY, USER_ONLY, FilterTemplateError::Empty),
            (
                "(uid=alice)",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::MissingPlaceholder("{username}".into()),
            ),
            (
                "(uid={user})",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::UnknownPlaceholder("user".into()),
            ),
            (
                "(&(uid={username})(member={dn}))",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::PlaceholderNotAllowed("dn"),
            ),
            (
                "(uid={username)",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::UnbalancedBrace,
            ),
            (
                "(uid=}{username})",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::UnbalancedBrace,
            ),
            (
                "(uid={username})(cn=x)",
                USER_ONLY,
                USER_ONLY,
                FilterTemplateError::InvalidFilter,
            ),
        ];
        for (text, allowed, required, expected) in cases {
            assert_eq!(
                FilterTemplate::parse(text, allowed, required).unwrap_err(),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_templates_that_are_not_a_filter() {
        for text in [
            "uid={username}",
            "(uid={username}",
            "(uid{username})",
            "((uid={username}))(",
        ] {
            assert_eq!(
                FilterTemplate::parse(text, USER_ONLY, USER_ONLY).unwrap_err(),
                FilterTemplateError::InvalidFilter,
                "{text}"
            );
        }
    }

    #[test]
    fn accepts_compound_templates_and_repeated_placeholders() {
        let template = user_template("(|(uid={username})(mail={username}))");
        let filter = template.render(Substitution {
            username: "a*b",
            dn: "",
        });
        assert_eq!(filter, "(|(uid=a\\2ab)(mail=a\\2ab))");
    }

    #[test]
    fn unescape_rejects_raw_specials_and_bad_escapes() {
        assert_eq!(unescape_filter_value("a(b"), None);
        assert_eq!(unescape_filter_value("a*b"), None);
        assert_eq!(unescape_filter_value("a\\zzb"), None);
        assert_eq!(unescape_filter_value("a\\5"), None);
        assert_eq!(unescape_filter_value("\\c3\\28"), None);
    }
}
