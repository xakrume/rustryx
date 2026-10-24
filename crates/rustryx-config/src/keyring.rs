//! Key rings: the current key plus the retired keys that still verify or open old data.
//!
//! Two rings exist. The internal JWT ring (`auth.internal_jwt_key`, `auth.internal_jwt_key_id`,
//! `auth.internal_jwt_previous_keys`) and the sealing ring (`database.sealing_key`,
//! `database.sealing_key_id`, `database.sealing_previous_keys`). Both follow one rotation
//! procedure: deploy the new key as current with the old key listed as previous, wait until
//! nothing signed or sealed with the old key is left (one session lifetime for JWTs, the
//! reseal job for sealed values), then remove the old entry.
//!
//! This module only parses and checks the text. Building the actual key objects belongs to
//! `rustryx-auth` and `rustryx-db`, so no error here ever contains key material: a message
//! names the key id or the position of the entry, never a value.

use rustryx_core::Secret;

use crate::schema::{AuthConfig, DatabaseConfig};

/// Retired keys kept next to the current one. Same bound as `rustryx-auth`'s `KeyRing`: more
/// than that means a rotation was never finished.
pub const MAX_PREVIOUS_KEYS: usize = 8;
/// Longest key id of the JWT ring (the bound `rustryx-auth` enforces on `kid`).
pub const MAX_JWT_KEY_ID_CHARS: usize = 64;
/// Longest key id of the sealing ring (the bound `rustryx-db`'s `KeyEntry` enforces).
pub const MAX_SEALING_KEY_ID_CHARS: usize = 32;
/// Length of a sealing key in hexadecimal characters (AES-256).
pub const SEALING_KEY_HEX_CHARS: usize = 64;

/// Why a key ring text is unusable. The message never repeats key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyRingError {
    /// A key id is empty, too long or uses characters outside `[A-Za-z0-9._-]`.
    #[error(
        "entry {0}: the key id must be 1 to {1} characters of letters, digits, '.', '_' or '-'"
    )]
    InvalidId(usize, usize),
    /// Two keys of one ring share an id.
    #[error("entry {0}: the key id is used twice in the ring")]
    DuplicateId(usize),
    /// An entry has no key, or the key has the wrong shape.
    #[error("entry {0}: {1}")]
    InvalidKey(usize, &'static str),
    /// More than [`MAX_PREVIOUS_KEYS`] retired keys.
    #[error("more than {MAX_PREVIOUS_KEYS} retired keys: finish an earlier rotation first")]
    TooMany,
}

/// The sealing ring as configured.
#[derive(Debug, Clone)]
pub struct SealingRing {
    /// Id of the key new values are sealed with.
    pub current_id: String,
    /// The key, 64 hexadecimal characters.
    pub current: Secret,
    /// Retired keys `(id, hex key)` that still open old values.
    pub previous: Vec<(String, Secret)>,
}

/// The internal JWT ring as configured.
#[derive(Debug, Clone)]
pub struct JwtRing {
    /// `kid` of the signing key.
    pub current_id: String,
    /// PKCS#8 PEM of the signing key.
    pub current: Secret,
    /// Retired keys `(kid, PEM)` that still verify tokens (public keys are preferred).
    pub previous: Vec<(String, Secret)>,
}

/// True for an id of 1 to `max_chars` characters of `[A-Za-z0-9._-]`. The JWT ring accepts
/// `.`; the sealing ring does not (its envelope is split on `.`), see [`valid_sealing_id`].
fn valid_id(id: &str, max_chars: usize, allow_dot: bool) -> bool {
    !id.is_empty()
        && id.chars().count() <= max_chars
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (allow_dot && c == '.'))
}

/// A JWT key id as `rustryx-auth` accepts it.
#[must_use]
pub fn valid_jwt_id(id: &str) -> bool {
    valid_id(id, MAX_JWT_KEY_ID_CHARS, true)
}

/// A sealing key id as `rustryx-db` accepts it. No dot: the sealed envelope
/// `rx1.<key id>.<body>` is split on dots, so a dotted id would never be found again.
#[must_use]
pub fn valid_sealing_id(id: &str) -> bool {
    valid_id(id, MAX_SEALING_KEY_ID_CHARS, false)
}

fn is_hex_key(text: &str) -> bool {
    text.len() == SEALING_KEY_HEX_CHARS && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parses `database.sealing_previous_keys`: `id:hex` entries separated by commas, spaces or
/// new lines. Blank text is an empty list.
///
/// # Errors
/// [`KeyRingError`] naming the (1-based) entry that is malformed.
pub fn parse_sealing_previous(text: &str) -> Result<Vec<(String, String)>, KeyRingError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let items = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|item| !item.is_empty());
    for (index, item) in items.enumerate() {
        let position = index + 1;
        let Some((id, key)) = item.split_once(':') else {
            return Err(KeyRingError::InvalidKey(position, "expected id:key"));
        };
        if !valid_sealing_id(id) {
            return Err(KeyRingError::InvalidId(position, MAX_SEALING_KEY_ID_CHARS));
        }
        if !is_hex_key(key) {
            return Err(KeyRingError::InvalidKey(
                position,
                "the key must be 64 hexadecimal characters",
            ));
        }
        if entries.iter().any(|(known, _)| known == id) {
            return Err(KeyRingError::DuplicateId(position));
        }
        entries.push((id.to_owned(), key.to_owned()));
    }
    if entries.len() > MAX_PREVIOUS_KEYS {
        return Err(KeyRingError::TooMany);
    }
    Ok(entries)
}

/// Parses `auth.internal_jwt_previous_keys`: a sequence of entries, each a line `kid: <id>`
/// followed by one PEM block. Blank text is an empty list.
///
/// ```text
/// kid: k1
/// -----BEGIN PUBLIC KEY-----
/// ...
/// -----END PUBLIC KEY-----
/// ```
///
/// # Errors
/// [`KeyRingError`] naming the (1-based) entry that is malformed.
pub fn parse_jwt_previous(text: &str) -> Result<Vec<(String, String)>, KeyRingError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in text.lines().map(str::trim_end) {
        if let Some(id) = line.trim_start().strip_prefix("kid:") {
            if let Some(done) = open.take() {
                push_jwt_entry(&mut entries, done)?;
            }
            open = Some((id.trim().to_owned(), String::new()));
        } else if let Some((_, pem)) = open.as_mut() {
            pem.push_str(line);
            pem.push('\n');
        } else if !line.trim().is_empty() {
            // A PEM block before any `kid:` line cannot be given an id.
            return Err(KeyRingError::InvalidKey(
                entries.len() + 1,
                "a key must be preceded by a `kid: <id>` line",
            ));
        }
    }
    if let Some(done) = open {
        push_jwt_entry(&mut entries, done)?;
    }
    if entries.len() > MAX_PREVIOUS_KEYS {
        return Err(KeyRingError::TooMany);
    }
    Ok(entries)
}

fn push_jwt_entry(
    entries: &mut Vec<(String, String)>,
    (id, pem): (String, String),
) -> Result<(), KeyRingError> {
    let position = entries.len() + 1;
    if !valid_jwt_id(&id) {
        return Err(KeyRingError::InvalidId(position, MAX_JWT_KEY_ID_CHARS));
    }
    let pem = pem.trim().to_owned();
    let framed = pem.starts_with("-----BEGIN ") && pem.ends_with("-----");
    if !framed || !pem.contains("-----END ") {
        return Err(KeyRingError::InvalidKey(
            position,
            "expected one PEM block after the kid line",
        ));
    }
    if pem.matches("-----BEGIN ").count() != 1 {
        return Err(KeyRingError::InvalidKey(
            position,
            "each kid line takes exactly one PEM block",
        ));
    }
    if entries.iter().any(|(known, _)| *known == id) {
        return Err(KeyRingError::DuplicateId(position));
    }
    entries.push((id, pem));
    Ok(())
}

impl DatabaseConfig {
    /// The sealing ring, `None` when no sealing key is configured.
    ///
    /// # Errors
    /// [`KeyRingError`] when `sealing_previous_keys` is malformed (validation reports the same).
    pub fn sealing_ring(&self) -> Result<Option<SealingRing>, KeyRingError> {
        let Some(current) = self.sealing_key.as_ref() else {
            return Ok(None);
        };
        let previous = match self.sealing_previous_keys.as_ref() {
            Some(text) => parse_sealing_previous(text.expose())?,
            None => Vec::new(),
        };
        Ok(Some(SealingRing {
            current_id: self.sealing_key_id.clone(),
            current: current.to_secret(),
            previous: previous
                .into_iter()
                .map(|(id, key)| (id, Secret::new(key)))
                .collect(),
        }))
    }
}

impl AuthConfig {
    /// The internal JWT ring, `None` when no signing key is configured.
    ///
    /// # Errors
    /// [`KeyRingError`] when `internal_jwt_previous_keys` is malformed.
    pub fn jwt_ring(&self) -> Result<Option<JwtRing>, KeyRingError> {
        let Some(current) = self.internal_jwt_key.as_ref() else {
            return Ok(None);
        };
        let previous = match self.internal_jwt_previous_keys.as_ref() {
            Some(text) => parse_jwt_previous(text.expose())?,
            None => Vec::new(),
        };
        Ok(Some(JwtRing {
            current_id: self.internal_jwt_key_id.clone(),
            current: current.to_secret(),
            previous: previous
                .into_iter()
                .map(|(id, pem)| (id, Secret::new(pem)))
                .collect(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::ConfigSecret;

    const HEX_A: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const HEX_B: &str = "0000000000000000000000000000000000000000000000000000000000000002";

    fn pem(label: &str, body: &str) -> String {
        format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----")
    }

    #[test]
    fn sealing_entries_split_on_commas_spaces_and_lines() {
        let text = format!("k0:{HEX_A}, k9:{HEX_B}\n");
        let parsed = parse_sealing_previous(&text).expect("parses");
        assert_eq!(
            parsed,
            vec![
                ("k0".to_owned(), HEX_A.to_owned()),
                ("k9".to_owned(), HEX_B.to_owned())
            ]
        );
        assert_eq!(parse_sealing_previous("  \n ").expect("blank"), Vec::new());
    }

    #[test]
    fn sealing_entries_reject_every_malformed_shape_without_echoing_the_key() {
        for bad in [
            "no-colon".to_owned(),
            format!(":{HEX_A}"),
            format!("a.b:{HEX_A}"),
            format!("{}:{HEX_A}", "x".repeat(33)),
            "k1:short".to_owned(),
            format!("k1:{}", "z".repeat(64)),
            format!("k1:{HEX_A},k1:{HEX_B}"),
        ] {
            let error = parse_sealing_previous(&bad).expect_err(&bad);
            let shown = error.to_string();
            assert!(!shown.contains(HEX_A) && !shown.contains(HEX_B), "{shown}");
        }
    }

    #[test]
    fn sealing_ring_has_a_ceiling() {
        let many: Vec<String> = (0..=MAX_PREVIOUS_KEYS)
            .map(|n| format!("k{n}:{HEX_A}"))
            .collect();
        assert_eq!(
            parse_sealing_previous(&many.join(",")),
            Err(KeyRingError::TooMany)
        );
    }

    #[test]
    fn jwt_entries_pair_each_kid_with_one_pem_block() {
        let text = format!(
            "kid: k1\n{}\n\nkid: k0\n{}\n",
            pem("PUBLIC KEY", "AAAA"),
            pem("PRIVATE KEY", "BBBB")
        );
        let parsed = parse_jwt_previous(&text).expect("parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "k1");
        assert!(parsed[0].1.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert_eq!(parsed[1].0, "k0");
        assert_eq!(parse_jwt_previous("\n").expect("blank"), Vec::new());
    }

    #[test]
    fn jwt_entries_reject_missing_kid_missing_pem_and_duplicates() {
        let block = pem("PUBLIC KEY", "AAAA");
        for bad in [
            block.clone(),
            "kid: k1".to_owned(),
            format!("kid: k1\n{block}\nkid: k1\n{block}"),
            format!("kid: bad id\n{block}"),
            format!("kid: \n{block}"),
            format!("kid: k1\n{block}\n{block}"),
            "kid: k1\nnot a pem".to_owned(),
        ] {
            assert!(parse_jwt_previous(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn jwt_ring_has_a_ceiling() {
        let block = pem("PUBLIC KEY", "AAAA");
        let many: Vec<String> = (0..=MAX_PREVIOUS_KEYS)
            .map(|n| format!("kid: k{n}\n{block}"))
            .collect();
        assert_eq!(
            parse_jwt_previous(&many.join("\n")),
            Err(KeyRingError::TooMany)
        );
    }

    #[test]
    fn id_rules_match_the_crates_that_consume_them() {
        assert!(valid_jwt_id("2026-10.a_b"));
        assert!(!valid_jwt_id(""));
        assert!(!valid_jwt_id(&"k".repeat(65)));
        assert!(valid_sealing_id("k1_x-2"));
        assert!(
            !valid_sealing_id("k1.x"),
            "a dot would break the envelope split"
        );
        assert!(!valid_sealing_id(&"k".repeat(33)));
    }

    #[test]
    fn rings_come_out_of_the_config_with_their_ids() {
        let mut config = crate::AppConfig::default();
        assert!(config.database.sealing_ring().expect("ok").is_none());
        assert!(config.auth.jwt_ring().expect("ok").is_none());

        config.database.sealing_key = Some(ConfigSecret::new(HEX_A));
        config.database.sealing_key_id = "k2".into();
        config.database.sealing_previous_keys = Some(ConfigSecret::new(format!("k1:{HEX_B}")));
        let ring = config
            .database
            .sealing_ring()
            .expect("ok")
            .expect("configured");
        assert_eq!(ring.current_id, "k2");
        assert_eq!(ring.current.expose(), HEX_A);
        assert_eq!(ring.previous.len(), 1);
        assert_eq!(ring.previous[0].0, "k1");

        config.auth.internal_jwt_key = Some(ConfigSecret::new(pem("PRIVATE KEY", "CCCC")));
        config.auth.internal_jwt_key_id = "j2".into();
        config.auth.internal_jwt_previous_keys = Some(ConfigSecret::new(format!(
            "kid: j1\n{}",
            pem("PUBLIC KEY", "DDDD")
        )));
        let jwt = config.auth.jwt_ring().expect("ok").expect("configured");
        assert_eq!(jwt.current_id, "j2");
        assert_eq!(jwt.previous[0].0, "j1");
    }

    #[test]
    fn a_malformed_previous_list_is_an_error_not_an_empty_ring() {
        let mut config = crate::AppConfig::default();
        config.database.sealing_key = Some(ConfigSecret::new(HEX_A));
        config.database.sealing_previous_keys = Some(ConfigSecret::new("garbage"));
        assert!(config.database.sealing_ring().is_err());
        config.auth.internal_jwt_key = Some(ConfigSecret::new(pem("PRIVATE KEY", "CCCC")));
        config.auth.internal_jwt_previous_keys = Some(ConfigSecret::new("garbage"));
        assert!(config.auth.jwt_ring().is_err());
    }
}
