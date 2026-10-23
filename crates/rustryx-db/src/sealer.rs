//! Sealing of secrets that must be stored (upstream credentials of proxy repositories).
//!
//! The database never holds such a secret in clear text: a dump or a replica would leak it
//! (SEC-25). The store is given a [`SecretSealer`] at construction. [`AeadSealer`] is the
//! built-in implementation (AES-256-GCM). A KMS or Vault Transit backed implementation can
//! be plugged in through the trait without touching this crate.

use std::fmt;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustryx_core::{Secret, StoreError};
use zeroize::Zeroize;

use crate::error::backend;

const ENVELOPE_PREFIX: &str = "rx1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const MAX_KEY_ID_LEN: usize = 32;

/// Seals and opens short secrets, binding each ciphertext to a context (`aad`).
pub trait SecretSealer: Send + Sync + fmt::Debug {
    /// Encrypts `plaintext` and returns a self-describing text envelope. `aad` is
    /// authenticated but not stored: opening with a different `aad` must fail, which is what
    /// stops a sealed value copied to another row from being usable there.
    fn seal(&self, aad: &[u8], plaintext: &str) -> Result<String, StoreError>;

    /// Decrypts an envelope produced by [`SecretSealer::seal`] with the same `aad`.
    fn open(&self, aad: &[u8], envelope: &str) -> Result<Secret, StoreError>;
}

/// One named AES-256 key. `Debug` shows the id only.
pub struct KeyEntry {
    id: String,
    cipher: Aes256Gcm,
}

impl KeyEntry {
    /// Builds a key from raw bytes. `id` is stored in every envelope so that the right key
    /// is found after a rotation; it must be 1..=32 characters of `[A-Za-z0-9_-]`.
    pub fn from_bytes(id: &str, mut key: [u8; KEY_LEN]) -> Result<Self, StoreError> {
        let valid_id = !id.is_empty()
            && id.len() <= MAX_KEY_ID_LEN
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid_id {
            key.zeroize();
            return Err(backend("build sealing key", "invalid key id"));
        }
        let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key));
        key.zeroize();
        Ok(Self {
            id: id.to_owned(),
            cipher,
        })
    }

    /// Builds a key from 64 hexadecimal characters (what a mounted secret file holds).
    pub fn from_hex(id: &str, hex_key: &Secret) -> Result<Self, StoreError> {
        let mut bytes = [0_u8; KEY_LEN];
        if hex::decode_to_slice(hex_key.expose().trim(), &mut bytes).is_err() {
            bytes.zeroize();
            return Err(backend(
                "build sealing key",
                "key must be 64 hexadecimal characters",
            ));
        }
        Self::from_bytes(id, bytes)
    }
}

impl fmt::Debug for KeyEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyEntry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// AES-256-GCM sealer with a key ring.
///
/// New envelopes use the current key; older keys stay in the ring only to open envelopes
/// sealed before a rotation. A random 96 bit nonce per envelope is safe for the volume here
/// (one envelope per proxy repository edit), far below the 2^32 limit for random nonces.
///
/// Envelope: `rx1.<key id>.<base64url(nonce || ciphertext || tag)>`.
pub struct AeadSealer {
    current: KeyEntry,
    previous: Vec<KeyEntry>,
}

impl AeadSealer {
    /// Creates a sealer. Key ids must be unique across `current` and `previous`.
    pub fn new(current: KeyEntry, previous: Vec<KeyEntry>) -> Result<Self, StoreError> {
        let mut ids: Vec<&str> = previous.iter().map(|k| k.id.as_str()).collect();
        ids.push(current.id.as_str());
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(backend("build sealer", "duplicate key id in the key ring"));
        }
        Ok(Self { current, previous })
    }

    /// Id of the key new envelopes are sealed with.
    #[must_use]
    pub fn current_key_id(&self) -> &str {
        &self.current.id
    }

    fn key(&self, id: &str) -> Option<&KeyEntry> {
        if self.current.id == id {
            return Some(&self.current);
        }
        self.previous.iter().find(|k| k.id == id)
    }
}

impl fmt::Debug for AeadSealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let previous: Vec<&str> = self.previous.iter().map(|k| k.id.as_str()).collect();
        f.debug_struct("AeadSealer")
            .field("current", &self.current.id)
            .field("previous", &previous)
            .finish()
    }
}

/// The only error a caller ever sees from `open`: a wrong key, a wrong context, a damaged
/// envelope and a truncated one are indistinguishable on purpose.
fn open_failed() -> StoreError {
    backend("open sealed secret", "sealed secret cannot be opened")
}

impl SecretSealer for AeadSealer {
    fn seal(&self, aad: &[u8], plaintext: &str) -> Result<String, StoreError> {
        let mut nonce_bytes = [0_u8; NONCE_LEN];
        rand::fill(&mut nonce_bytes);
        let nonce = Nonce::<aes_gcm::aes::cipher::consts::U12>::from(nonce_bytes);
        let ciphertext = self
            .current
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| backend("seal secret", "encryption failed"))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(format!(
            "{ENVELOPE_PREFIX}.{}.{}",
            self.current.id,
            URL_SAFE_NO_PAD.encode(blob)
        ))
    }

    fn open(&self, aad: &[u8], envelope: &str) -> Result<Secret, StoreError> {
        let mut parts = envelope.splitn(3, '.');
        let (Some(ENVELOPE_PREFIX), Some(key_id), Some(body)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(open_failed());
        };
        let key = self.key(key_id).ok_or_else(open_failed)?;
        let blob = URL_SAFE_NO_PAD.decode(body).map_err(|_| open_failed())?;
        if blob.len() <= NONCE_LEN {
            return Err(open_failed());
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce_array: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| open_failed())?;
        let nonce = Nonce::<aes_gcm::aes::cipher::consts::U12>::from(nonce_array);
        let plaintext = key
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| open_failed())?;
        let text = String::from_utf8(plaintext).map_err(|_| open_failed())?;
        Ok(Secret::new(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer() -> AeadSealer {
        AeadSealer::new(KeyEntry::from_bytes("k1", [7; 32]).unwrap(), vec![]).unwrap()
    }

    #[test]
    fn round_trip_and_nonce_is_fresh() {
        let s = sealer();
        let a = s.seal(b"ctx", "hunter2").unwrap();
        let b = s.seal(b"ctx", "hunter2").unwrap();
        assert_ne!(a, b);
        assert_eq!(s.open(b"ctx", &a).unwrap().expose(), "hunter2");
        assert!(a.starts_with("rx1.k1."));
    }

    #[test]
    fn must_not_open_in_another_context() {
        let s = sealer();
        let sealed = s.seal(b"repo-a", "hunter2").unwrap();
        assert!(s.open(b"repo-b", &sealed).is_err());
    }

    #[test]
    fn must_not_contain_plaintext_in_envelope() {
        let sealed = sealer().seal(b"c", "hunter2-canary").unwrap();
        assert!(!sealed.contains("hunter2"));
    }

    #[test]
    fn rejects_damaged_envelopes_uniformly() {
        let s = sealer();
        let good = s.seal(b"c", "x").unwrap();
        // Flip one character inside the ciphertext. The replacement always differs from the
        // original, otherwise a tamper that picked the same character would "open" fine.
        let mut chars: Vec<char> = good.chars().collect();
        let pos = chars.len() - 5;
        chars[pos] = if chars[pos] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        for bad in [
            "",
            "rx1",
            "rx1.k1",
            "rx1.k1.",
            "rx1.zz.AAAA",
            "rx2.k1.AAAA",
            tampered.as_str(),
        ] {
            let err = s.open(b"c", bad).unwrap_err();
            assert_eq!(
                err.to_string(),
                "store backend failure during open sealed secret"
            );
        }
    }

    #[test]
    fn rotation_opens_old_envelopes_and_seals_with_current() {
        let old = AeadSealer::new(KeyEntry::from_bytes("old", [1; 32]).unwrap(), vec![]).unwrap();
        let sealed = old.seal(b"c", "pw").unwrap();
        let rotated = AeadSealer::new(
            KeyEntry::from_bytes("new", [2; 32]).unwrap(),
            vec![KeyEntry::from_bytes("old", [1; 32]).unwrap()],
        )
        .unwrap();
        assert_eq!(rotated.open(b"c", &sealed).unwrap().expose(), "pw");
        assert!(rotated.seal(b"c", "pw").unwrap().starts_with("rx1.new."));
        assert!(old.open(b"c", &rotated.seal(b"c", "pw").unwrap()).is_err());
    }

    #[test]
    fn key_validation() {
        assert!(KeyEntry::from_bytes("", [0; 32]).is_err());
        assert!(KeyEntry::from_bytes("a.b", [0; 32]).is_err());
        assert!(KeyEntry::from_bytes(&"x".repeat(33), [0; 32]).is_err());
        assert!(KeyEntry::from_hex("k", &Secret::new("zz")).is_err());
        let hex_key = Secret::new("00".repeat(32));
        assert!(KeyEntry::from_hex("k", &hex_key).is_ok());
        let dup = AeadSealer::new(
            KeyEntry::from_bytes("k", [0; 32]).unwrap(),
            vec![KeyEntry::from_bytes("k", [1; 32]).unwrap()],
        );
        assert!(dup.is_err());
    }

    #[test]
    fn must_not_print_key_material_in_debug() {
        let rendered = format!("{:?}", sealer());
        assert!(rendered.contains("k1"));
        assert!(!rendered.contains('7'));
    }
}
