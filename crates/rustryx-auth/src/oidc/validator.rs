//! Validation of Keycloak access tokens (bearer) and ID tokens (code flow) with `jsonwebtoken`.
//!
//! The algorithm is never taken from the token alone: it must be in the allowlist
//! (RS256, ES256) and it must be the one the JWKS key was published for, so an
//! HS256 token "signed" with the public key as secret cannot pass, and `none`
//! never reaches the crypto layer. `kid` is required, and `jku`, `x5u`, `x5c` and
//! `jwk` headers are never read: keys come only from the HTTPS JWKS of the
//! configured issuer (README 8.3).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, Validation, decode, get_current_timestamp};
use rustryx_core::AuthError;
use serde::Deserialize;
use subtle::ConstantTimeEq;

use super::claims::{ClaimError, ClaimsStats, GroupPolicy, OidcIdentity, RawClaims, identity_from};
use super::config::{OidcConfig, SCOPE_OFFLINE_ACCESS};
use super::jwks::{JwksCache, KeyError};

const MAX_KID_LEN: usize = 256;

/// Why a token was refused. Reasons are for logs; clients only see 401 or 503.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenError {
    /// Not a JWT with an allowed algorithm and the configured issuer: not ours to judge.
    NotApplicable,
    Malformed,
    Algorithm,
    MissingKid,
    UnknownKey,
    KeyUnavailable,
    KeyMismatch,
    Signature,
    Expired,
    NotYetValid,
    Issuer,
    Audience,
    MissingClaim,
    AuthorizedParty,
    TokenType,
    IdTokenAsBearer,
    OfflineToken,
    Nonce,
    Claims,
}

impl TokenError {
    /// Stable, secret-free reason for logs and `AuthError::InvalidToken`.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::NotApplicable => "not an oidc token",
            Self::Malformed => "malformed token",
            Self::Algorithm => "algorithm not allowed",
            Self::MissingKid => "kid is required",
            Self::UnknownKey => "signing key unknown",
            Self::KeyUnavailable => "signing keys unavailable",
            Self::KeyMismatch => "key does not match algorithm",
            Self::Signature => "signature verification failed",
            Self::Expired => "token expired",
            Self::NotYetValid => "token not yet valid",
            Self::Issuer => "issuer mismatch",
            Self::Audience => "audience mismatch",
            Self::MissingClaim => "required claim missing",
            Self::AuthorizedParty => "authorized party mismatch",
            Self::TokenType => "unexpected token type",
            Self::IdTokenAsBearer => "id token is not an access token",
            Self::OfflineToken => "offline tokens are not accepted",
            Self::Nonce => "nonce mismatch",
            Self::Claims => "invalid claims",
        }
    }
}

impl From<TokenError> for AuthError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::Expired => Self::TokenExpired,
            TokenError::KeyUnavailable => {
                Self::ProviderUnavailable("oidc signing keys are not available".to_owned())
            }
            other => Self::InvalidToken(other.reason()),
        }
    }
}

/// A token that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedToken {
    pub(crate) identity: OidcIdentity,
    /// Unix seconds of `exp`.
    pub(crate) expires_at: i64,
}

/// What a token is verified as.
enum Profile<'a> {
    /// Keycloak access token presented as `Authorization: Bearer`.
    Access,
    /// ID token returned by the token endpoint of the UI client.
    Id { client_id: &'a str, nonce: &'a str },
}

#[derive(Deserialize)]
struct RawHeader {
    alg: Option<String>,
    kid: Option<String>,
}

#[derive(Deserialize)]
struct IssuerOnly {
    iss: Option<serde_json::Value>,
}

/// First look at a token, before any key is fetched: is it ours, and which key does it name.
struct Peeked {
    alg: Algorithm,
    kid: Option<String>,
}

enum PeekFailure {
    NotJwt,
    AlgorithmNotAllowed,
    OtherIssuer,
}

/// Shared validation pipeline of one issuer.
pub(crate) struct TokenValidator {
    jwks: Arc<JwksCache>,
    expected_issuer: String,
    audience: String,
    azp_allowed: Vec<String>,
    skew: Duration,
    groups: GroupPolicy,
    max_token_bytes: usize,
    stats: Arc<ClaimsStats>,
}

impl TokenValidator {
    pub(crate) fn new(config: &OidcConfig, jwks: Arc<JwksCache>, stats: Arc<ClaimsStats>) -> Self {
        Self {
            jwks,
            expected_issuer: config.expected_issuer().to_owned(),
            audience: config.audience.clone(),
            azp_allowed: config.azp_allowed(),
            skew: config.clock_skew,
            groups: GroupPolicy {
                claim: config.groups_claim.clone(),
                max_groups: config.max_groups,
                max_group_bytes: config.max_group_bytes,
            },
            max_token_bytes: config.max_token_bytes,
            stats,
        }
    }

    /// Verifies a Keycloak access token.
    pub(crate) async fn validate_access(&self, token: &str) -> Result<VerifiedToken, TokenError> {
        self.verify(token, &Profile::Access).await
    }

    /// Verifies the ID token of the UI client: signature, issuer, audience, expiry and nonce.
    pub(crate) async fn validate_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
    ) -> Result<VerifiedToken, TokenError> {
        self.verify(token, &Profile::Id { client_id, nonce }).await
    }

    async fn verify(
        &self,
        token: &str,
        profile: &Profile<'_>,
    ) -> Result<VerifiedToken, TokenError> {
        let peeked = self
            .peek(token)
            .map_err(|failure| match (profile, failure) {
                (Profile::Access, _) => TokenError::NotApplicable,
                (Profile::Id { .. }, PeekFailure::NotJwt) => TokenError::Malformed,
                (Profile::Id { .. }, PeekFailure::AlgorithmNotAllowed) => TokenError::Algorithm,
                (Profile::Id { .. }, PeekFailure::OtherIssuer) => TokenError::Issuer,
            })?;
        let kid = peeked.kid.ok_or(TokenError::MissingKid)?;
        let key = self.jwks.key_for(&kid).await.map_err(|error| match error {
            KeyError::UnknownKid => TokenError::UnknownKey,
            KeyError::Unavailable => TokenError::KeyUnavailable,
        })?;
        // The key was published for one algorithm; a token naming another one is a
        // key confusion attempt, not a verification failure to retry.
        if key.alg != peeked.alg {
            return Err(TokenError::KeyMismatch);
        }
        let audience = match profile {
            Profile::Access => self.audience.as_str(),
            Profile::Id { client_id, .. } => client_id,
        };
        let data = decode::<RawClaims>(token, &key.key, &self.validation(peeked.alg, audience))
            .map_err(|error| map_jwt_error(error.kind()))?;
        let claims = data.claims;
        match profile {
            Profile::Access => self.check_access_token(&claims)?,
            Profile::Id { client_id, nonce } => check_id_token(&claims, client_id, nonce)?,
        }
        self.check_issued_at(&claims)?;
        let expires_at = claims
            .exp
            .as_ref()
            .and_then(serde_json::Number::as_i64)
            .ok_or(TokenError::MissingClaim)?;
        let identity = identity_from(&claims, &self.expected_issuer, &self.groups, &self.stats)
            .map_err(|error| match error {
                ClaimError::MissingSubject => TokenError::MissingClaim,
                ClaimError::InvalidValue => TokenError::Claims,
            })?;
        Ok(VerifiedToken {
            identity,
            expires_at,
        })
    }

    /// Cheap structural check done before any key lookup or network access.
    fn peek(&self, token: &str) -> Result<Peeked, PeekFailure> {
        if token.len() > self.max_token_bytes {
            return Err(PeekFailure::NotJwt);
        }
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(PeekFailure::NotJwt);
        };
        let header: RawHeader = decode_part(header).ok_or(PeekFailure::NotJwt)?;
        let alg = match header.alg.as_deref() {
            Some("RS256") => Algorithm::RS256,
            Some("ES256") => Algorithm::ES256,
            // `none`, HS*, RS384, PS*, EdDSA, unknown or missing: never verified here. Checked
            // before the signature so that an unsigned token is reported as what it is.
            _ => return Err(PeekFailure::AlgorithmNotAllowed),
        };
        if signature.is_empty() {
            return Err(PeekFailure::NotJwt);
        }
        let issuer: IssuerOnly = decode_part(payload).ok_or(PeekFailure::NotJwt)?;
        match issuer.iss {
            Some(serde_json::Value::String(iss)) if iss == self.expected_issuer => {}
            _ => return Err(PeekFailure::OtherIssuer),
        }
        let kid = header
            .kid
            .filter(|kid| !kid.is_empty() && kid.len() <= MAX_KID_LEN);
        Ok(Peeked { alg, kid })
    }

    /// `iss`, `aud`, `exp`, `nbf` and the signature are enforced by `jsonwebtoken`;
    /// `iss` and `aud` are compared as exact strings.
    fn validation(&self, alg: Algorithm, audience: &str) -> Validation {
        let mut validation = Validation::new(alg);
        validation.leeway = self.skew.as_secs();
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.validate_aud = true;
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        validation.set_issuer(&[self.expected_issuer.as_str()]);
        validation.set_audience(&[audience]);
        validation
    }

    fn check_access_token(&self, claims: &RawClaims) -> Result<(), TokenError> {
        match claims.typ.as_deref() {
            Some(typ) if typ.eq_ignore_ascii_case("Bearer") => {}
            // An ID token carries the product client id as `aud` too: without this check
            // it would pass as an access token.
            Some(typ) if typ.eq_ignore_ascii_case("ID") => return Err(TokenError::IdTokenAsBearer),
            Some(typ) if typ.eq_ignore_ascii_case("Offline") => {
                return Err(TokenError::OfflineToken);
            }
            _ => return Err(TokenError::TokenType),
        }
        let offline = claims
            .scope
            .as_deref()
            .is_some_and(|scope| scope.split_whitespace().any(|s| s == SCOPE_OFFLINE_ACCESS));
        if offline {
            return Err(TokenError::OfflineToken);
        }
        match claims.azp.as_deref() {
            Some(azp) if !self.azp_allowed.iter().any(|allowed| allowed == azp) => {
                Err(TokenError::AuthorizedParty)
            }
            _ => Ok(()),
        }
    }

    /// `jsonwebtoken` validates `nbf` but not `iat`: a token issued in the future is as
    /// suspicious as one that is not valid yet.
    fn check_issued_at(&self, claims: &RawClaims) -> Result<(), TokenError> {
        let Some(iat) = claims.iat.as_ref().and_then(serde_json::Number::as_f64) else {
            return Ok(());
        };
        #[allow(clippy::cast_precision_loss)] // timestamps are far below 2^53
        let latest = (get_current_timestamp() + self.skew.as_secs()) as f64;
        if iat > latest {
            return Err(TokenError::NotYetValid);
        }
        Ok(())
    }
}

fn check_id_token(claims: &RawClaims, client_id: &str, nonce: &str) -> Result<(), TokenError> {
    if !claims
        .typ
        .as_deref()
        .is_some_and(|typ| typ.eq_ignore_ascii_case("ID"))
    {
        return Err(TokenError::TokenType);
    }
    if claims.azp.as_deref().is_some_and(|azp| azp != client_id) {
        return Err(TokenError::AuthorizedParty);
    }
    let presented = claims.nonce.as_deref().unwrap_or_default();
    // Constant time: the nonce is a secret of the browser session that started the flow.
    if nonce.is_empty() || !bool::from(presented.as_bytes().ct_eq(nonce.as_bytes())) {
        return Err(TokenError::Nonce);
    }
    Ok(())
}

fn decode_part<T: for<'de> Deserialize<'de>>(part: &str) -> Option<T> {
    let bytes = URL_SAFE_NO_PAD.decode(part).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn map_jwt_error(kind: &ErrorKind) -> TokenError {
    match kind {
        ErrorKind::ExpiredSignature => TokenError::Expired,
        ErrorKind::ImmatureSignature => TokenError::NotYetValid,
        ErrorKind::InvalidIssuer => TokenError::Issuer,
        ErrorKind::InvalidAudience => TokenError::Audience,
        ErrorKind::InvalidSignature => TokenError::Signature,
        ErrorKind::MissingRequiredClaim(_) => TokenError::MissingClaim,
        ErrorKind::InvalidAlgorithm
        | ErrorKind::InvalidAlgorithmName
        | ErrorKind::UnsupportedAlgorithm
        | ErrorKind::MissingAlgorithm => TokenError::Algorithm,
        ErrorKind::InvalidClaimFormat(_) => TokenError::Claims,
        // A JWKS entry that jsonwebtoken cannot use is not a key for this token.
        ErrorKind::InvalidKeyFormat
        | ErrorKind::InvalidRsaKey(_)
        | ErrorKind::InvalidEcdsaKey
        | ErrorKind::InvalidEddsaKey => TokenError::KeyMismatch,
        _ => TokenError::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_errors_map_to_auth_errors() {
        assert!(matches!(
            AuthError::from(TokenError::Expired),
            AuthError::TokenExpired
        ));
        assert!(matches!(
            AuthError::from(TokenError::KeyUnavailable),
            AuthError::ProviderUnavailable(_)
        ));
        assert!(matches!(
            AuthError::from(TokenError::Audience),
            AuthError::InvalidToken("audience mismatch")
        ));
    }

    #[test]
    fn every_reason_is_a_non_empty_distinct_string() {
        let all = [
            TokenError::NotApplicable,
            TokenError::Malformed,
            TokenError::Algorithm,
            TokenError::MissingKid,
            TokenError::UnknownKey,
            TokenError::KeyUnavailable,
            TokenError::KeyMismatch,
            TokenError::Signature,
            TokenError::Expired,
            TokenError::NotYetValid,
            TokenError::Issuer,
            TokenError::Audience,
            TokenError::MissingClaim,
            TokenError::AuthorizedParty,
            TokenError::TokenType,
            TokenError::IdTokenAsBearer,
            TokenError::OfflineToken,
            TokenError::Nonce,
            TokenError::Claims,
        ];
        let mut reasons: Vec<_> = all.iter().map(|e| e.reason()).collect();
        assert!(reasons.iter().all(|r| !r.is_empty()));
        reasons.sort_unstable();
        reasons.dedup();
        assert_eq!(reasons.len(), all.len());
    }

    #[test]
    fn jwt_errors_map_to_token_errors() {
        assert_eq!(
            map_jwt_error(&ErrorKind::ExpiredSignature),
            TokenError::Expired
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::ImmatureSignature),
            TokenError::NotYetValid
        );
        assert_eq!(map_jwt_error(&ErrorKind::InvalidIssuer), TokenError::Issuer);
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidAudience),
            TokenError::Audience
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidSignature),
            TokenError::Signature
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::MissingRequiredClaim("exp".into())),
            TokenError::MissingClaim
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidAlgorithm),
            TokenError::Algorithm
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidToken),
            TokenError::Malformed
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::UnsupportedAlgorithm),
            TokenError::Algorithm
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidClaimFormat("exp".into())),
            TokenError::Claims
        );
        assert_eq!(
            map_jwt_error(&ErrorKind::InvalidKeyFormat),
            TokenError::KeyMismatch
        );
    }
}
