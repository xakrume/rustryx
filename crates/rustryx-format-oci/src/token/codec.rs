//! `HmacTokenCodec`: registry tokens as compact JWT-shaped strings signed with HMAC-SHA-256.
//!
//! This is the reference [`TokenIssuer`] and [`TokenVerifier`]. A deployment with a key ring
//! (the server's internal JWT) implements the two traits over that instead; the service and
//! the tests do not care which.
//!
//! Rules that make it safe to verify tokens a hostile client sends back:
//!
//! * the algorithm is fixed: the header must say `HS256`, `none` and everything else are
//!   refused, and nothing in the header (`jku`, `x5u`, `kid`, `jwk`) is ever followed;
//! * the signature is checked, in constant time, BEFORE the payload is parsed, so an
//!   unauthenticated payload is never interpreted;
//! * `exp`, `nbf` and `iat` are checked against the clock the caller passes, with a 5 second
//!   leeway for clock skew between nodes, and the audience and issuer must match exactly;
//! * a token longer than 8 KiB is refused before it is decoded.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use rustryx_core::ids::UserId;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use time::OffsetDateTime;
use uuid::Uuid;

use super::claims::{
    AccessGrant, GrantResource, RegistryTokenClaims, TokenError, TokenIssuer, TokenVerifier,
};
use crate::parse::scope::Action;

type HmacSha256 = Hmac<Sha256>;

/// Shortest signing key accepted: the size of the HMAC-SHA-256 block's security level.
pub const MIN_KEY_LEN: usize = 32;
/// Longest token verified.
const MAX_TOKEN_LEN: usize = 8 * 1024;
/// Allowed clock difference between the node that issued a token and the one that checks it.
const LEEWAY_SECS: i64 = 5;

/// Issues and verifies HS256 registry tokens with one shared key.
#[derive(Clone)]
pub struct HmacTokenCodec {
    key: Vec<u8>,
    issuer: String,
    service: String,
}

impl std::fmt::Debug for HmacTokenCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key never reaches a log through `{:?}`.
        f.debug_struct("HmacTokenCodec")
            .field("key", &"[REDACTED]")
            .field("issuer", &self.issuer)
            .field("service", &self.service)
            .finish()
    }
}

impl HmacTokenCodec {
    /// Builds a codec. `issuer` is what it puts in and expects as `iss`, `service` what it
    /// expects as `aud` (the service name of the registry).
    ///
    /// # Errors
    /// [`TokenError::KeyUnusable`] when the key is shorter than [`MIN_KEY_LEN`].
    pub fn new(
        key: &[u8],
        issuer: impl Into<String>,
        service: impl Into<String>,
    ) -> Result<Self, TokenError> {
        if key.len() < MIN_KEY_LEN {
            return Err(TokenError::KeyUnusable);
        }
        Ok(Self {
            key: key.to_vec(),
            issuer: issuer.into(),
            service: service.into(),
        })
    }

    fn mac(&self) -> Result<HmacSha256, TokenError> {
        HmacSha256::new_from_slice(&self.key).map_err(|_| TokenError::KeyUnusable)
    }
}

impl TokenIssuer for HmacTokenCodec {
    fn issue(&self, claims: &RegistryTokenClaims) -> Result<String, TokenError> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let access: Vec<Value> = claims.access.iter().map(grant_json).collect();
        let mut payload = json!({
            "iss": claims.issuer,
            "sub": claims.subject,
            "aud": claims.audience,
            "iat": claims.issued_at.unix_timestamp(),
            "nbf": claims.not_before.unix_timestamp(),
            "exp": claims.expires_at.unix_timestamp(),
            "jti": claims.token_id,
            "access": access,
        });
        if let Some(id) = claims.subject_id {
            payload["uid"] = Value::String(id.to_string());
        }
        let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
        let signing_input = format!("{header}.{payload}");
        let mut mac = self.mac()?;
        mac.update(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        Ok(format!("{signing_input}.{signature}"))
    }
}

impl TokenVerifier for HmacTokenCodec {
    fn verify(&self, token: &str, now: OffsetDateTime) -> Result<RegistryTokenClaims, TokenError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Malformed);
        }
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        check_header(header)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| TokenError::Malformed)?;
        let mut mac = self.mac()?;
        mac.update(header.as_bytes());
        mac.update(b".");
        mac.update(payload.as_bytes());
        // `verify_slice` compares in constant time.
        mac.verify_slice(&signature)
            .map_err(|_| TokenError::BadSignature)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| TokenError::Malformed)?;
        let raw: RawClaims = serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed)?;
        self.check_claims(raw, now)
    }
}

impl HmacTokenCodec {
    fn check_claims(
        &self,
        raw: RawClaims,
        now: OffsetDateTime,
    ) -> Result<RegistryTokenClaims, TokenError> {
        if raw.iss != self.issuer {
            return Err(TokenError::WrongIssuer);
        }
        if raw.aud != self.service {
            return Err(TokenError::WrongAudience);
        }
        let at = |seconds: i64| {
            OffsetDateTime::from_unix_timestamp(seconds).map_err(|_| TokenError::Malformed)
        };
        let (issued_at, not_before, expires_at) = (at(raw.iat)?, at(raw.nbf)?, at(raw.exp)?);
        let leeway = time::Duration::seconds(LEEWAY_SECS);
        if now >= expires_at {
            return Err(TokenError::Expired);
        }
        if now + leeway < not_before || now + leeway < issued_at {
            return Err(TokenError::NotYetValid);
        }
        let access = raw
            .access
            .into_iter()
            .map(RawGrant::into_grant)
            .collect::<Result<Vec<_>, _>>()?;
        let subject_id = match raw.uid.as_deref() {
            None => None,
            Some(text) => Some(
                Uuid::parse_str(text)
                    .map(UserId::from_uuid)
                    .map_err(|_| TokenError::Malformed)?,
            ),
        };
        Ok(RegistryTokenClaims {
            issuer: raw.iss,
            subject: raw.sub,
            subject_id,
            audience: raw.aud,
            issued_at,
            not_before,
            expires_at,
            token_id: raw.jti,
            access,
        })
    }
}

fn check_header(header: &str) -> Result<(), TokenError> {
    #[derive(Deserialize)]
    struct Header {
        alg: String,
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| TokenError::Malformed)?;
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| TokenError::Malformed)?;
    if header.alg == "HS256" {
        Ok(())
    } else {
        Err(TokenError::UnsupportedAlgorithm)
    }
}

fn grant_json(grant: &AccessGrant) -> Value {
    let actions: Vec<&str> = grant.actions.iter().map(|a| a.as_str()).collect();
    match &grant.resource {
        GrantResource::Repository(name) => {
            json!({ "type": "repository", "name": name, "actions": actions })
        }
        GrantResource::Catalog => {
            json!({ "type": "registry", "name": "catalog", "actions": actions })
        }
    }
}

#[derive(Deserialize)]
struct RawClaims {
    iss: String,
    sub: String,
    aud: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    jti: String,
    uid: Option<String>,
    access: Vec<RawGrant>,
}

#[derive(Deserialize)]
struct RawGrant {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    actions: Vec<String>,
}

impl RawGrant {
    fn into_grant(self) -> Result<AccessGrant, TokenError> {
        let resource = match (self.kind.as_str(), self.name.as_str()) {
            ("repository", name) => GrantResource::Repository(name.to_owned()),
            ("registry", "catalog") => GrantResource::Catalog,
            _ => return Err(TokenError::Malformed),
        };
        let actions = self
            .actions
            .iter()
            .filter_map(|word| Action::parse(word))
            .collect();
        Ok(AccessGrant { resource, actions })
    }
}

#[cfg(test)]
mod tests;
