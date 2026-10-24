//! CSRF tokens: a keyed hash of the session id, double submitted.
//!
//! The token of a session is `base64url(HMAC-SHA256(key, tid))`. It is set as the script
//! readable cookie `__Host-rustryx_csrf` and returned by `GET /auth/session` and
//! `GET /auth/csrf`; the page sends it back as `X-CSRF-Token`. A request is accepted when the
//! header equals the cookie AND the value is the hash of the session's own `tid`:
//!
//! - an attacker page cannot read the cookie (it is `SameSite=Strict` and cross-origin), and
//!   cannot compute the hash without the key;
//! - a token of another session or of an older token id (the id changes on every reissue)
//!   does not match, so a captured value dies with its session token.
//!
//! The key is derived from `auth.cookie_key`, which is already a secret shared by all nodes,
//! with a fixed label so that the same bytes never serve two purposes.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use rustryx_core::secret::Secret;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Label mixed into the key derivation: one key, one purpose.
const KEY_LABEL: &[u8] = b"rustryx/csrf/v1";

/// The key material is not 32 bytes of key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsrfKeyError;

impl std::fmt::Display for CsrfKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the cookie key must be 32 bytes as base64 or 64 hexadecimal characters")
    }
}

impl std::error::Error for CsrfKeyError {}

/// The key of the CSRF tokens.
#[derive(Clone)]
pub struct CsrfKey {
    /// A keyed, empty HMAC state; cloned for every token so nothing is re-keyed.
    template: HmacSha256,
}

impl std::fmt::Debug for CsrfKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CsrfKey([REDACTED])")
    }
}

impl CsrfKey {
    /// Derives the key from `auth.cookie_key`: base64 (any alphabet, padded or not) or 64
    /// hexadecimal characters, 32 bytes, not all equal (what an unconfigured template looks
    /// like).
    ///
    /// # Errors
    /// [`CsrfKeyError`] when the secret is not 32 bytes of key material.
    pub fn from_cookie_key(secret: &Secret) -> Result<Self, CsrfKeyError> {
        let raw = secret.expose().trim();
        let decoded = if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            hex_decode(raw)
        } else {
            [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
                .iter()
                .find_map(|engine| engine.decode(raw).ok())
        };
        let bytes = decoded
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or(CsrfKeyError)?;
        if bytes.iter().all(|b| *b == bytes[0]) {
            return Err(CsrfKeyError);
        }
        let mut derive = HmacSha256::new_from_slice(&bytes).map_err(|_| CsrfKeyError)?;
        derive.update(KEY_LABEL);
        let derived = derive.finalize().into_bytes();
        let template = HmacSha256::new_from_slice(&derived).map_err(|_| CsrfKeyError)?;
        Ok(Self { template })
    }

    /// The token of the session with this token id.
    #[must_use]
    pub fn token_for(&self, tid: &str) -> String {
        let mut mac = self.template.clone();
        mac.update(tid.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// True when `presented` is the token of `tid`. Constant time.
    #[must_use]
    pub fn verifies(&self, tid: &str, presented: &str) -> bool {
        let expected = self.token_for(tid);
        bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
    }
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(*pair.get(1)?).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests;
