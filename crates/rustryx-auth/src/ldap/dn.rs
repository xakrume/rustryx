//! Distinguished name helpers (RFC 4514).
//!
//! Two jobs live here: escaping a value that becomes part of a DN, and building a
//! comparison key for DNs that the directory spells differently.

use std::borrow::Cow;

/// Escapes a string for use as an attribute value inside a relative distinguished
/// name (RFC 4514 section 2.4).
///
/// `"`, `+`, `,`, `;`, `<`, `>` and `\` get a backslash, a leading space or `#` and a
/// trailing space get a backslash, and NUL becomes `\00`. The default login flow
/// never builds a DN from input (it searches, then binds as the DN the server
/// returned), so this exists for the day a DN has to carry a user supplied value:
/// raw concatenation is the bug this function prevents.
#[must_use]
pub fn escape_dn_value(value: &str) -> Cow<'_, str> {
    let last = value.chars().count().saturating_sub(1);
    let needs_escape = value.chars().enumerate().any(|(index, ch)| {
        is_always_escaped(ch)
            || ch == '\0'
            || (index == 0 && matches!(ch, ' ' | '#'))
            || (index == last && ch == ' ')
    });
    if !needs_escape {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len() + 8);
    for (index, ch) in value.chars().enumerate() {
        match ch {
            '\0' => escaped.push_str("\\00"),
            c if is_always_escaped(c) => {
                escaped.push('\\');
                escaped.push(c);
            }
            c @ (' ' | '#') if index == 0 => {
                escaped.push('\\');
                escaped.push(c);
            }
            ' ' if index == last => escaped.push_str("\\ "),
            other => escaped.push(other),
        }
    }
    Cow::Owned(escaped)
}

const fn is_always_escaped(ch: char) -> bool {
    matches!(ch, '"' | '+' | ',' | ';' | '<' | '>' | '\\')
}

/// Builds a comparison key for a DN: lower case, with the insignificant spaces
/// around `,`, `;`, `+` and `=` removed and `;` folded into `,`.
///
/// The key is used only for the visited set of the group walk. Directories return
/// the same DN with different case or spacing depending on the attribute it came
/// from (`memberOf` versus an entry DN), so comparing raw strings would let a cycle
/// run until the depth limit. A weak key can only make the walk visit a group twice,
/// never skip one, and the depth and size limits stay in force either way.
#[must_use]
pub(crate) fn dn_key(dn: &str) -> String {
    let mut key = String::with_capacity(dn.len());
    let mut trailing_spaces = 0usize;
    let mut chars = dn.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                key.extend(ch.to_lowercase());
                if let Some(escaped) = chars.next() {
                    key.extend(escaped.to_lowercase());
                }
                trailing_spaces = 0;
            }
            ',' | ';' | '+' | '=' => {
                key.truncate(key.len() - trailing_spaces);
                key.push(if ch == ';' { ',' } else { ch });
                trailing_spaces = 0;
                while chars.peek() == Some(&' ') {
                    chars.next();
                }
            }
            ' ' => {
                key.push(' ');
                trailing_spaces += 1;
            }
            other => {
                key.extend(other.to_lowercase());
                trailing_spaces = 0;
            }
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Decodes `\<char>` and `\hh` escapes, the inverse of [`escape_dn_value`].
    fn unescape(escaped: &str) -> Option<String> {
        let mut out: Vec<u8> = Vec::new();
        let mut chars = escaped.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                continue;
            }
            let next = chars.next()?;
            if next.is_ascii_hexdigit() && chars.peek().is_some_and(char::is_ascii_hexdigit) {
                let low = chars.next()?;
                let byte = u8::from_str_radix(&format!("{next}{low}"), 16).ok()?;
                out.push(byte);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(next.encode_utf8(&mut buf).as_bytes());
            }
        }
        String::from_utf8(out).ok()
    }

    #[test]
    fn escapes_the_rfc4514_examples() {
        assert_eq!(
            escape_dn_value("James \"Jim\" Smith, III"),
            "James \\\"Jim\\\" Smith\\, III"
        );
        assert_eq!(escape_dn_value("a+b;c<d>e\\f"), "a\\+b\\;c\\<d\\>e\\\\f");
        assert_eq!(escape_dn_value("#hash"), "\\#hash");
        assert_eq!(escape_dn_value(" lead"), "\\ lead");
        assert_eq!(escape_dn_value("trail "), "trail\\ ");
        assert_eq!(escape_dn_value(" "), "\\ ");
        assert_eq!(escape_dn_value("nul\0byte"), "nul\\00byte");
    }

    #[test]
    fn leaves_ordinary_values_and_inner_special_positions_alone() {
        for plain in ["alice", "", "a b", "a#b", "Zoë", "a=b", "名前"] {
            assert!(
                matches!(escape_dn_value(plain), Cow::Borrowed(_)),
                "{plain}"
            );
        }
    }

    /// Counts RDNs the way a server splits them: on commas that are not escaped.
    fn rdn_count(dn: &str) -> usize {
        let mut count = 1;
        let mut chars = dn.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => {
                    chars.next();
                }
                ',' => count += 1,
                _ => {}
            }
        }
        count
    }

    #[test]
    fn must_not_let_an_escaped_value_add_an_rdn_or_change_the_value() {
        let injected = "x,cn=admin,dc=example,dc=org";
        let dn = format!(
            "uid={},ou=people,dc=example,dc=org",
            escape_dn_value(injected)
        );
        assert_eq!(rdn_count(&dn), 4);
        assert!(dn.starts_with("uid=x\\,cn=admin\\,dc=example\\,dc=org,ou=people"));
    }

    proptest! {
        #[test]
        fn escaped_dn_values_round_trip(chars in prop::collection::vec(
            prop_oneof![any::<char>(), prop::sample::select(vec![' ', '#', ',', '+', '\\', '"', '\0', ';', '<', '>'])],
            0..40,
        )) {
            let value: String = chars.into_iter().collect();
            let escaped = escape_dn_value(&value);
            prop_assert_eq!(unescape(&escaped), Some(value.clone()));
            let dn = format!("uid={escaped},dc=x");
            prop_assert_eq!(rdn_count(&dn), 2, "value {:?} changed the RDN structure", value);
        }
    }

    #[test]
    fn dn_key_ignores_case_and_spacing_around_separators() {
        let a = dn_key("CN=Admins , OU=Groups ; DC=Example, DC=Org");
        let b = dn_key("cn=admins,ou=groups,dc=example,dc=org");
        assert_eq!(a, b);
        assert_eq!(dn_key("cn=a+uid=b, dc=x"), dn_key("CN=A + UID=B,DC=X"));
    }

    #[test]
    fn dn_key_keeps_escaped_separators_and_spaces() {
        assert_eq!(dn_key("cn=Smith\\, John,dc=x"), "cn=smith\\, john,dc=x");
        assert_ne!(dn_key("cn=a\\,b,dc=x"), dn_key("cn=a,b,dc=x"));
        assert_eq!(dn_key("cn=a\\ ,dc=x"), "cn=a\\ ,dc=x");
        assert_eq!(dn_key("  cn=a  "), "cn=a");
        assert_eq!(dn_key("cn=a\\"), "cn=a\\");
    }

    #[test]
    fn dn_key_of_distinct_groups_differs() {
        assert_ne!(dn_key("cn=a,dc=x"), dn_key("cn=b,dc=x"));
        assert_ne!(dn_key("cn=a,dc=x"), dn_key("cn=a,dc=y"));
    }
}
