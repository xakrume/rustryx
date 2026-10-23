//! Ed25519 key ring for the internal JWT: one signing key, several verify-only keys.
//!
//! Rotation procedure: generate a new key, deploy it as `current` with the old
//! key listed in `previous`, wait one maximum session lifetime (8 hours), then
//! drop the old key. Tokens signed by the old key keep verifying in between, and
//! nothing new is signed with it. The `kid` header selects the key, so a token
//! can never be verified with a key it did not name.

use ed25519_dalek::SigningKey;
use ed25519_dalek::VerifyingKey;
use ed25519_dalek::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey};
use jsonwebtoken::{DecodingKey, EncodingKey};
use rustryx_core::Secret;

/// Verify-only keys kept next to the current one.
pub const MAX_PREVIOUS_KEYS: usize = 8;
/// Longest accepted key id.
const MAX_KID_CHARS: usize = 64;

/// Why a key ring could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The key id is empty, too long or has characters outside `[A-Za-z0-9._-]`.
    #[error("invalid key id")]
    InvalidKid,
    /// Two keys share one id.
    #[error("duplicate key id `{0}`")]
    DuplicateKid(String),
    /// The PEM text is not a usable Ed25519 key.
    #[error("key `{0}` is not a valid Ed25519 PKCS#8 private key or SPKI public key in PEM form")]
    InvalidPem(String),
    /// More than [`MAX_PREVIOUS_KEYS`] previous keys.
    #[error("too many previous keys")]
    TooManyPrevious,
    /// The signing key could not be converted for the JWT library.
    #[error("signing key could not be encoded")]
    Encoding,
}

fn check_kid(kid: &str) -> Result<(), KeyError> {
    let valid = !kid.is_empty()
        && kid.chars().count() <= MAX_KID_CHARS
        && kid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(KeyError::InvalidKid)
    }
}

struct VerifyEntry {
    kid: String,
    key: DecodingKey,
}

/// The current signing key and every key still accepted for verification.
pub struct KeyRing {
    signing_kid: String,
    encoding: EncodingKey,
    /// Current key first, then the previous ones.
    verify: Vec<VerifyEntry>,
}

impl std::fmt::Debug for KeyRing {
    /// Key ids only: the keys themselves must never reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRing")
            .field("current", &self.signing_kid)
            .field(
                "verify_kids",
                &self
                    .verify
                    .iter()
                    .map(|e| e.kid.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl KeyRing {
    /// Builds a ring from PEM text.
    ///
    /// `current` is a PKCS#8 private key. `previous` entries may be SPKI public
    /// keys (preferred: nothing secret has to stay around) or PKCS#8 private keys
    /// of retired signing keys.
    pub fn from_pem(
        current_kid: &str,
        current_private_pem: &Secret,
        previous: &[(String, Secret)],
    ) -> Result<Self, KeyError> {
        let signing = SigningKey::from_pkcs8_pem(current_private_pem.expose())
            .map_err(|_| KeyError::InvalidPem(current_kid.to_owned()))?;
        let mut previous_keys = Vec::with_capacity(previous.len());
        for (kid, pem) in previous {
            previous_keys.push((kid.clone(), Self::verifying_from_pem(kid, pem.expose())?));
        }
        Self::assemble(current_kid, &signing, previous_keys)
    }

    fn verifying_from_pem(kid: &str, pem: &str) -> Result<VerifyingKey, KeyError> {
        VerifyingKey::from_public_key_pem(pem)
            .or_else(|_| SigningKey::from_pkcs8_pem(pem).map(|key| key.verifying_key()))
            .map_err(|_| KeyError::InvalidPem(kid.to_owned()))
    }

    /// Builds a ring from a raw 32 byte seed (tests and bootstrap tooling).
    pub fn from_seed(
        kid: &str,
        seed: &[u8; 32],
        previous: &[(String, VerifyingKey)],
    ) -> Result<Self, KeyError> {
        Self::assemble(kid, &SigningKey::from_bytes(seed), previous.to_vec())
    }

    /// Generates a fresh random key. For development and first-start bootstrap;
    /// production deployments mount the same key on every node.
    pub fn generate(kid: &str) -> Result<Self, KeyError> {
        let mut seed = [0_u8; 32];
        rand::fill(&mut seed);
        Self::from_seed(kid, &seed, &[])
    }

    fn assemble(
        kid: &str,
        signing: &SigningKey,
        previous: Vec<(String, VerifyingKey)>,
    ) -> Result<Self, KeyError> {
        check_kid(kid)?;
        if previous.len() > MAX_PREVIOUS_KEYS {
            return Err(KeyError::TooManyPrevious);
        }
        let der = signing.to_pkcs8_der().map_err(|_| KeyError::Encoding)?;
        let mut verify = vec![VerifyEntry {
            kid: kid.to_owned(),
            key: DecodingKey::from_ed_der(signing.verifying_key().as_bytes()),
        }];
        for (previous_kid, key) in previous {
            check_kid(&previous_kid)?;
            if verify.iter().any(|entry| entry.kid == previous_kid) {
                return Err(KeyError::DuplicateKid(previous_kid));
            }
            verify.push(VerifyEntry {
                kid: previous_kid,
                key: DecodingKey::from_ed_der(key.as_bytes()),
            });
        }
        Ok(Self {
            signing_kid: kid.to_owned(),
            encoding: EncodingKey::from_ed_der(der.as_bytes()),
            verify,
        })
    }

    /// Id of the key new tokens are signed with.
    #[must_use]
    pub fn current_kid(&self) -> &str {
        &self.signing_kid
    }

    /// Ids of every key accepted for verification, current first.
    #[must_use]
    pub fn kids(&self) -> Vec<&str> {
        self.verify.iter().map(|entry| entry.kid.as_str()).collect()
    }

    pub(crate) const fn encoding_key(&self) -> &EncodingKey {
        &self.encoding
    }

    /// The verification key named by `kid`, if the ring knows it.
    pub(crate) fn decoding_key(&self, kid: &str) -> Option<&DecodingKey> {
        self.verify
            .iter()
            .find(|entry| entry.kid == kid)
            .map(|entry| &entry.key)
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::pkcs8::EncodePublicKey;
    use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;

    use super::*;

    fn seed(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn private_pem(byte: u8) -> Secret {
        let pem = SigningKey::from_bytes(&seed(byte))
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        Secret::new(pem.to_string())
    }

    fn public_pem(byte: u8) -> Secret {
        let pem = SigningKey::from_bytes(&seed(byte))
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        Secret::new(pem)
    }

    #[test]
    fn builds_from_pem_with_public_and_private_previous_keys() {
        let ring = KeyRing::from_pem(
            "k3",
            &private_pem(3),
            &[("k2".into(), public_pem(2)), ("k1".into(), private_pem(1))],
        )
        .unwrap();
        assert_eq!(ring.current_kid(), "k3");
        assert_eq!(ring.kids(), ["k3", "k2", "k1"]);
        assert!(ring.decoding_key("k2").is_some());
        assert!(ring.decoding_key("k9").is_none());
    }

    #[test]
    fn rejects_garbage_pem() {
        let bad = Secret::new("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----");
        assert_eq!(
            KeyRing::from_pem("k", &bad, &[]).unwrap_err(),
            KeyError::InvalidPem("k".into())
        );
        let previous = [("old".to_owned(), Secret::new("not a pem"))];
        assert_eq!(
            KeyRing::from_pem("k", &private_pem(1), &previous).unwrap_err(),
            KeyError::InvalidPem("old".into())
        );
    }

    #[test]
    fn rejects_bad_and_duplicate_kids() {
        for kid in ["", "has space", "slash/ed", &"k".repeat(65), "über"] {
            assert_eq!(
                KeyRing::from_seed(kid, &seed(1), &[]).unwrap_err(),
                KeyError::InvalidKid,
                "{kid:?}"
            );
        }
        let key = SigningKey::from_bytes(&seed(2)).verifying_key();
        let duplicate = [("k1".to_owned(), key)];
        assert_eq!(
            KeyRing::from_seed("k1", &seed(1), &duplicate).unwrap_err(),
            KeyError::DuplicateKid("k1".into())
        );
        let twice = [("p".to_owned(), key), ("p".to_owned(), key)];
        assert_eq!(
            KeyRing::from_seed("k1", &seed(1), &twice).unwrap_err(),
            KeyError::DuplicateKid("p".into())
        );
    }

    #[test]
    fn caps_the_number_of_previous_keys() {
        let key = SigningKey::from_bytes(&seed(2)).verifying_key();
        let many: Vec<(String, VerifyingKey)> = (0..=MAX_PREVIOUS_KEYS)
            .map(|i| (format!("p{i}"), key))
            .collect();
        assert_eq!(
            KeyRing::from_seed("k", &seed(1), &many).unwrap_err(),
            KeyError::TooManyPrevious
        );
    }

    #[test]
    fn generated_keys_differ() {
        let a = KeyRing::generate("a").unwrap();
        let b = KeyRing::generate("a").unwrap();
        // Ed25519 signatures are deterministic per key, so two different keys signing the
        // same message give different tokens.
        let sign = |ring: &KeyRing| {
            jsonwebtoken::encode(
                &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
                &serde_json::json!({"probe": 1}),
                ring.encoding_key(),
            )
            .unwrap()
        };
        assert_ne!(sign(&a), sign(&b));
        assert_eq!(sign(&a), sign(&a));
    }

    #[test]
    fn must_not_print_key_material_in_debug() {
        let ring = KeyRing::from_seed("visible-kid", &seed(7), &[]).unwrap();
        let rendered = format!("{ring:?}");
        assert!(rendered.contains("visible-kid"));
        // The seed bytes would show up as decimal 7s or hex 07s if a key leaked.
        assert!(!rendered.contains("[7, 7"));
        assert!(!rendered.contains("0707"));
    }
}
