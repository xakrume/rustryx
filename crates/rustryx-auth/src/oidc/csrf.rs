//! CSRF token helpers for the UI session.
//!
//! The pattern is a synchronizer token: the server keeps one random token in the
//! session, the UI echoes it in a header of every state-changing request, and the
//! server compares the two. `SameSite=Lax` on the session cookie is a second layer,
//! not a substitute: it does not cover same-site subdomains.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use subtle::ConstantTimeEq;

/// Entropy of a token: 256 bits.
const TOKEN_BYTES: usize = 32;

/// A fresh unpredictable CSRF token (URL-safe, no padding, 43 characters).
#[must_use]
pub fn generate_csrf_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes[..]);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compares the token kept in the session with the one the request presented, in constant
/// time. An empty expected token never matches: a session without a token must not
/// accept a request without one.
#[must_use]
pub fn verify_csrf_token(expected: &str, presented: &str) -> bool {
    !expected.is_empty() && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_long_enough() {
        let (a, b) = (generate_csrf_token(), generate_csrf_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
    }

    #[test]
    fn verifies_only_equal_non_empty_tokens() {
        let token = generate_csrf_token();
        assert!(verify_csrf_token(&token, &token));
        assert!(!verify_csrf_token(&token, &generate_csrf_token()));
        assert!(!verify_csrf_token(&token, ""));
        assert!(!verify_csrf_token(&token, &token[..42]));
    }

    #[test]
    fn must_not_accept_empty_expected_token() {
        assert!(!verify_csrf_token("", ""));
        assert!(!verify_csrf_token("", "anything"));
    }
}
