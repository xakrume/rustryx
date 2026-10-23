//! AES-256-GCM sealed cookie: stateless storage for the authorization-code flow.
//!
//! The browser carries `state`, `nonce` and the PKCE verifier between the redirect
//! to Keycloak and the callback. Sealing them (instead of keeping them in a server
//! side store) lets any node answer the callback in an HA deployment without a
//! shared store (README 8.3).
//!
//! Invariants and what holds them:
//! - confidentiality and integrity: AES-256-GCM, so any bit flip fails the tag;
//! - no nonce reuse: a fresh random 96-bit nonce per seal, and the key is only
//!   used for short-lived flow cookies, far below the 2^32 messages bound of
//!   random-nonce GCM;
//! - expiry cannot be extended by the client: the deadline sits inside the
//!   authenticated plaintext, `Max-Age` is only a browser hint;
//! - a value cannot move between purposes: the cookie name is bound into the tag
//!   as associated data.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::{Aead, Nonce, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use rand::RngExt;
use rustryx_core::Secret;
use rustryx_core::ports::Clock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Format version prefix of a sealed value. A new format gets a new prefix.
const VERSION_PREFIX: &str = "v1.";
/// AEAD associated data prefix, followed by the cookie name.
const AAD_LABEL: &[u8] = b"rustryx/sealed-cookie/v1\0";
const NONCE_LEN: usize = 12;
/// Largest accepted cookie value. A flow cookie is well below 1 KiB, anything larger is garbage.
const MAX_VALUE_LEN: usize = 4096;

/// Failure to build the codec or to seal a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// The key is not 32 bytes, not valid hex or base64, or a placeholder.
    #[error("invalid cookie key")]
    Key,
    /// Serialization or encryption failed.
    #[error("cannot seal value")]
    Seal,
}

/// Failure to open a sealed value. The variants are for logs, a client only ever sees 400/401.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// Not a value this codec could have produced (prefix, base64, length, payload shape).
    #[error("malformed sealed value")]
    Malformed,
    /// The tag did not verify: tampered, wrong key or sealed for another cookie name.
    #[error("sealed value failed authentication")]
    Authentication,
    /// The authenticated deadline has passed.
    #[error("sealed value expired")]
    Expired,
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    /// Unix seconds after which the value is refused.
    exp: i64,
    data: T,
}

/// Seals and opens short-lived cookie values and renders their `Set-Cookie` headers.
#[derive(Clone)]
pub struct SealedCookie {
    cipher: Aes256Gcm,
    name: String,
    path: String,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for SealedCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedCookie")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("ttl", &self.ttl)
            .field("key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SealedCookie {
    /// Builds the codec for one cookie name.
    ///
    /// `key` is 32 bytes as 64 hex chars or base64. All-equal keys (`00..00`, `ff..ff`)
    /// are refused because they are what an unconfigured template looks like.
    pub fn new(
        key: &Secret,
        name: impl Into<String>,
        ttl: Duration,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, SealError> {
        let key = parse_key_32(key)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SealError::Key)?;
        Ok(Self {
            cipher,
            name: name.into(),
            path: "/".to_owned(),
            ttl,
            clock,
        })
    }

    /// Restricts the cookie to a path (the callback path keeps it off every other request).
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Cookie name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Lifetime of a freshly sealed value.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Seals `payload`; the result is safe to use as a cookie value without quoting.
    pub fn seal<T: Serialize>(&self, payload: &T) -> Result<String, SealError> {
        let ttl_secs = i64::try_from(self.ttl.as_secs()).map_err(|_| SealError::Seal)?;
        let envelope = Envelope {
            exp: self.clock.now().unix_timestamp().saturating_add(ttl_secs),
            data: payload,
        };
        let plaintext = serde_json::to_vec(&envelope).map_err(|_| SealError::Seal)?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill(&mut nonce_bytes[..]);
        let nonce = Nonce::<Aes256Gcm>::try_from(&nonce_bytes[..]).map_err(|_| SealError::Seal)?;
        let aad = self.aad();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Seal)?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(format!("{VERSION_PREFIX}{}", URL_SAFE_NO_PAD.encode(blob)))
    }

    /// Verifies and decrypts a value produced by [`Self::seal`] with the same key and name.
    pub fn open<T: DeserializeOwned>(&self, value: &str) -> Result<T, OpenError> {
        if value.len() > MAX_VALUE_LEN {
            return Err(OpenError::Malformed);
        }
        let encoded = value
            .strip_prefix(VERSION_PREFIX)
            .ok_or(OpenError::Malformed)?;
        let blob = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| OpenError::Malformed)?;
        // A GCM tag is 16 bytes: anything shorter than nonce + tag cannot be valid.
        if blob.len() < NONCE_LEN + 16 {
            return Err(OpenError::Malformed);
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| OpenError::Malformed)?;
        let aad = self.aad();
        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| OpenError::Authentication)?;
        let envelope: Envelope<T> =
            serde_json::from_slice(&plaintext).map_err(|_| OpenError::Malformed)?;
        if self.clock.now().unix_timestamp() >= envelope.exp {
            return Err(OpenError::Expired);
        }
        Ok(envelope.data)
    }

    /// `Set-Cookie` header value carrying `value`, with `HttpOnly; Secure; SameSite=Lax`.
    ///
    /// `Lax` (not `Strict`) because the callback is a cross-site top-level navigation
    /// from Keycloak; `Strict` would drop the cookie exactly when it is needed.
    /// `value` must come from [`Self::seal`] (token characters only).
    #[must_use]
    pub fn set_cookie_header(&self, value: &str) -> String {
        format!(
            "{}={}; Max-Age={}; Path={}; HttpOnly; Secure; SameSite=Lax",
            self.name,
            value,
            self.ttl.as_secs(),
            self.path
        )
    }

    /// `Set-Cookie` header value that makes the browser drop the cookie (after the callback).
    #[must_use]
    pub fn clear_cookie_header(&self) -> String {
        format!(
            "{}=; Max-Age=0; Path={}; HttpOnly; Secure; SameSite=Lax",
            self.name, self.path
        )
    }

    fn aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(AAD_LABEL.len() + self.name.len());
        aad.extend_from_slice(AAD_LABEL);
        aad.extend_from_slice(self.name.as_bytes());
        aad
    }
}

/// Decodes a 32-byte key given as 64 hex chars or base64 (standard or URL-safe, padded or not).
pub(crate) fn parse_key_32(secret: &Secret) -> Result<[u8; 32], SealError> {
    let raw = secret.expose().trim();
    let decoded = if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(raw).ok()
    } else {
        [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
            .iter()
            .find_map(|engine| engine.decode(raw).ok())
    };
    let key = decoded
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or(SealError::Key)?;
    if key.iter().all(|byte| *byte == key[0]) {
        return Err(SealError::Key);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use crate::oidc::test_support::ManualClock;

    use super::*;

    fn key() -> Secret {
        Secret::new(hex::encode((0u8..32).collect::<Vec<_>>()))
    }

    fn codec(clock: Arc<ManualClock>) -> SealedCookie {
        SealedCookie::new(&key(), "flow", Duration::from_secs(600), clock).unwrap()
    }

    #[test]
    fn round_trips_a_payload() {
        let c = codec(ManualClock::new(1_000));
        let value = c.seal(&"hello".to_owned()).unwrap();
        assert!(value.starts_with("v1."));
        assert_eq!(c.open::<String>(&value).unwrap(), "hello");
    }

    #[test]
    fn sealing_twice_uses_a_fresh_nonce() {
        let c = codec(ManualClock::new(1_000));
        let a = c.seal(&"same").unwrap();
        let b = c.seal(&"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn must_not_open_tampered_value() {
        let c = codec(ManualClock::new(1_000));
        let value = c.seal(&"payload").unwrap();
        let blob = URL_SAFE_NO_PAD.decode(&value[3..]).unwrap();
        for index in [0, NONCE_LEN, blob.len() / 2, blob.len() - 1] {
            let mut copy = blob.clone();
            copy[index] ^= 0x01;
            let forged = format!("v1.{}", URL_SAFE_NO_PAD.encode(copy));
            assert_eq!(c.open::<String>(&forged), Err(OpenError::Authentication));
        }
    }

    #[test]
    fn must_not_open_malformed_values() {
        let c = codec(ManualClock::new(1_000));
        let long = format!("v1.{}", "A".repeat(MAX_VALUE_LEN));
        for bad in [
            "",
            "v1.",
            "v2.AAAA",
            "garbage",
            "v1.!!!!",
            "v1.AAAA",
            long.as_str(),
        ] {
            assert_eq!(c.open::<String>(bad), Err(OpenError::Malformed), "{bad}");
        }
    }

    #[test]
    fn must_not_open_expired_value() {
        let clock = ManualClock::new(1_000);
        let c = codec(Arc::clone(&clock));
        let value = c.seal(&"payload").unwrap();
        clock.advance(599);
        assert!(c.open::<String>(&value).is_ok());
        clock.advance(1);
        assert_eq!(c.open::<String>(&value), Err(OpenError::Expired));
    }

    #[test]
    fn must_not_open_value_sealed_for_another_cookie_name() {
        let clock = ManualClock::new(1_000);
        let a = codec(Arc::clone(&clock));
        let b = SealedCookie::new(&key(), "other", Duration::from_secs(600), clock).unwrap();
        let value = a.seal(&"payload").unwrap();
        assert_eq!(b.open::<String>(&value), Err(OpenError::Authentication));
    }

    #[test]
    fn must_not_open_value_sealed_under_another_key() {
        let clock = ManualClock::new(1_000);
        let a = codec(Arc::clone(&clock));
        let other_key = Secret::new(hex::encode((1u8..33).collect::<Vec<_>>()));
        let b = SealedCookie::new(&other_key, "flow", Duration::from_secs(600), clock).unwrap();
        let value = a.seal(&"payload").unwrap();
        assert_eq!(b.open::<String>(&value), Err(OpenError::Authentication));
    }

    #[test]
    fn must_not_open_authentic_value_with_wrong_payload_shape() {
        let c = codec(ManualClock::new(1_000));
        let value = c.seal(&"a string").unwrap();
        assert_eq!(c.open::<Vec<u8>>(&value), Err(OpenError::Malformed));
    }

    #[test]
    fn rejects_bad_keys() {
        let clock = ManualClock::new(0);
        let bad = [
            String::new(),
            "short".to_owned(),
            "00".repeat(32),
            "ff".repeat(32),
            hex::encode((0u8..31).collect::<Vec<_>>()),
        ];
        for key in bad {
            let r = SealedCookie::new(
                &Secret::new(key),
                "n",
                Duration::from_secs(1),
                clock.clone(),
            );
            assert_eq!(r.unwrap_err(), SealError::Key);
        }
    }

    #[test]
    fn accepts_base64_keys_in_every_flavour() {
        let raw: Vec<u8> = (200u8..232).collect();
        for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
            let k = Secret::new(engine.encode(&raw));
            assert_eq!(parse_key_32(&k).unwrap().to_vec(), raw);
        }
    }

    #[test]
    fn header_carries_security_attributes() {
        let c = codec(ManualClock::new(0)).with_path("/oidc/callback");
        let header = c.set_cookie_header("v1.abc");
        assert_eq!(
            header,
            "flow=v1.abc; Max-Age=600; Path=/oidc/callback; HttpOnly; Secure; SameSite=Lax"
        );
        let cleared = c.clear_cookie_header();
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.contains("HttpOnly; Secure; SameSite=Lax"));
        assert_eq!(c.name(), "flow");
        assert_eq!(c.ttl(), Duration::from_secs(600));
    }

    #[test]
    fn debug_never_shows_key_material() {
        let c = codec(ManualClock::new(0));
        let shown = format!("{c:?}");
        assert!(shown.contains("REDACTED"));
        assert!(!shown.contains("000102"));
    }
}
