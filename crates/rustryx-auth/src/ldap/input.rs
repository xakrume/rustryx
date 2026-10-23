//! Checks on what the client typed, done before anything touches the network.

/// Longest password accepted, in bytes. Real passwords are far shorter; the cap only
/// keeps an abusive header from being pushed through a BER encoder and a TLS socket.
const MAX_PASSWORD_BYTES: usize = 4096;

/// Why an input was refused. The reason is for logs; the client always sees
/// `InvalidCredentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputRejection {
    EmptyLogin,
    LoginTooLong,
    LoginControlCharacter,
    EmptyPassword,
    BlankPassword,
    PasswordNul,
    PasswordTooLong,
}

impl InputRejection {
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::EmptyLogin => "empty login",
            Self::LoginTooLong => "login too long",
            Self::LoginControlCharacter => "login contains a control character",
            Self::EmptyPassword => "empty password",
            Self::BlankPassword => "password is only whitespace",
            Self::PasswordNul => "password contains a NUL byte",
            Self::PasswordTooLong => "password too long",
        }
    }
}

/// Validates a login name before it is substituted into a filter (SEC-08: length and
/// characters are checked first, escaping is the second line of defence).
pub(crate) fn check_login(login: &str, max_bytes: usize) -> Result<(), InputRejection> {
    if login.trim().is_empty() {
        return Err(InputRejection::EmptyLogin);
    }
    if login.len() > max_bytes {
        return Err(InputRejection::LoginTooLong);
    }
    if login.chars().any(char::is_control) {
        return Err(InputRejection::LoginControlCharacter);
    }
    Ok(())
}

/// Validates a password before any bind.
///
/// An empty password must never reach the wire: a simple bind with a DN and an empty
/// password is an "unauthenticated bind" (RFC 4513 section 5.1.2), which many servers
/// answer with success, so the user would be logged in without knowing anything. A
/// password of only spaces is refused for the same reason (some servers trim it to
/// empty), and NUL is refused because it can truncate the value inside C based servers.
pub(crate) fn check_password(password: &str) -> Result<(), InputRejection> {
    if password.is_empty() {
        return Err(InputRejection::EmptyPassword);
    }
    if password.chars().all(char::is_whitespace) {
        return Err(InputRejection::BlankPassword);
    }
    if password.contains('\0') {
        return Err(InputRejection::PasswordNul);
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(InputRejection::PasswordTooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_logins_and_passwords() {
        assert_eq!(check_login("alice", 256), Ok(()));
        assert_eq!(
            check_login("a*(b)\\c", 256),
            Ok(()),
            "specials are escaped later, not refused"
        );
        assert_eq!(check_login("Zoë Müller", 256), Ok(()));
        assert_eq!(check_password("correct horse battery staple"), Ok(()));
        assert_eq!(check_password(" leading and trailing "), Ok(()));
        assert_eq!(check_password("p\u{e4}ssw\u{f6}rd"), Ok(()));
    }

    #[test]
    fn must_not_accept_an_empty_blank_or_nul_password() {
        assert_eq!(check_password(""), Err(InputRejection::EmptyPassword));
        for blank in [" ", "   ", "\t", " \u{a0} ", "\n \r\n", "\u{3000}"] {
            assert_eq!(
                check_password(blank),
                Err(InputRejection::BlankPassword),
                "{blank:?}"
            );
        }
        for nul in ["\0", "abc\0", "\0abc", "a\0b"] {
            assert!(check_password(nul).is_err(), "{nul:?}");
        }
        assert_eq!(check_password("\0"), Err(InputRejection::PasswordNul));
        assert_eq!(check_password(" \0 "), Err(InputRejection::PasswordNul));
    }

    #[test]
    fn must_not_accept_an_oversized_password() {
        assert_eq!(check_password(&"a".repeat(4096)), Ok(()));
        assert_eq!(
            check_password(&"a".repeat(4097)),
            Err(InputRejection::PasswordTooLong)
        );
    }

    #[test]
    fn rejects_bad_logins() {
        assert_eq!(check_login("", 256), Err(InputRejection::EmptyLogin));
        assert_eq!(check_login("   ", 256), Err(InputRejection::EmptyLogin));
        assert_eq!(
            check_login(&"a".repeat(257), 256),
            Err(InputRejection::LoginTooLong)
        );
        assert_eq!(check_login(&"a".repeat(256), 256), Ok(()));
        for bad in ["a\0b", "a\nb", "a\rb", "a\tb", "\u{7f}", "a\u{85}b"] {
            assert_eq!(
                check_login(bad, 256),
                Err(InputRejection::LoginControlCharacter),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn every_rejection_has_a_distinct_reason() {
        let all = [
            InputRejection::EmptyLogin,
            InputRejection::LoginTooLong,
            InputRejection::LoginControlCharacter,
            InputRejection::EmptyPassword,
            InputRejection::BlankPassword,
            InputRejection::PasswordNul,
            InputRejection::PasswordTooLong,
        ];
        let mut reasons: Vec<&str> = all.iter().map(|r| r.reason()).collect();
        reasons.sort_unstable();
        reasons.dedup();
        assert_eq!(reasons.len(), all.len());
    }

    proptest::proptest! {
        #[test]
        fn a_password_is_accepted_exactly_when_it_is_usable(
            chars in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::any::<char>(),
                    proptest::sample::select(vec![' ', '\t', '\0', '\u{a0}', 'a', '1']),
                ],
                0..24,
            )
        ) {
            let password: String = chars.into_iter().collect();
            let usable = !password.is_empty()
                && !password.chars().all(char::is_whitespace)
                && !password.contains('\0');
            proptest::prop_assert_eq!(check_password(&password).is_ok(), usable);
        }
    }
}
