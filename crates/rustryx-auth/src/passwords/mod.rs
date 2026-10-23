//! Local password hashing: argon2id for new hashes, bcrypt verification for
//! imported hashes, and the admission control that keeps a login storm from
//! starving the runtime.
//!
//! # Parameters
//!
//! New hashes are argon2id v19 with a 16 byte random salt in the PHC string
//! format. Defaults are `m = 64 MiB, t = 3, p = 1` (the recommended profile of
//! the security spec). The accepted minimum is the OWASP floor `m = 19 MiB,
//! t = 2, p = 1`; [`PasswordConfig::validated`] refuses anything below it.
//! Memory budget at peak: `password permits x m`, for example 8 cores x 64 MiB =
//! 512 MiB. Size the pod limit for that, or lower `m` toward the floor.
//!
//! # Rehash
//!
//! [`Verification::Valid`] carries `needs_rehash`. It is true for every bcrypt
//! hash and for argon2id hashes whose `m`, `t` or `p` is below the configured
//! values (parameters are only ever raised, never lowered). The caller writes
//! the new hash through `UserRepo::set_password_hash` after a successful login.
//!
//! # Timing
//!
//! A login for an unknown account must cost the same as one for a known account
//! with a wrong password: [`Passwords::verify_dummy`] runs a real argon2id
//! verification, with the configured parameters, under the same semaphore.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the type and on `BlockingGate::run`

pub mod gate;

pub(crate) mod errmap;

use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::API_TOKEN_PREFIX;

use self::errmap::backend_failure;
pub use self::gate::{BlockingGate, GateClass, GateConfig, LaneLimits};
use std::sync::Arc;

/// Longest password (in bytes) the service hashes or verifies. Bounds the work one
/// request can ask for; it is also the policy limit for new passwords.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Shortest password accepted when a new one is set (SEC-09).
pub const MIN_PASSWORD_CHARS: usize = 12;

/// argon2id version 0x13, the only version this crate writes.
const ARGON2_VERSION: u32 = 0x13;
/// bcrypt prefixes accepted for imported hashes. `$2x$` is deliberately absent:
/// it marks hashes produced by a buggy implementation and must not be trusted.
const BCRYPT_PREFIXES: [&str; 3] = ["$2a$", "$2b$", "$2y$"];

/// True when the text can be a password at all: not empty and not whitespace only.
///
/// Checked before any hashing so that blank credentials never cost CPU and never
/// reach a verifier (LDAP treats an empty simple bind as anonymous, SEC-08).
#[must_use]
pub fn is_usable_password(password: &str) -> bool {
    !password.trim().is_empty()
}

/// Why a new password was refused by [`validate_new_password`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordPolicyError {
    /// Empty or whitespace only.
    #[error("password must not be blank")]
    Blank,
    /// Shorter than [`MIN_PASSWORD_CHARS`].
    #[error("password must have at least {MIN_PASSWORD_CHARS} characters")]
    TooShort,
    /// Longer than [`MAX_PASSWORD_BYTES`].
    #[error("password must not exceed {MAX_PASSWORD_BYTES} bytes")]
    TooLong,
    /// Starts with the API token prefix, which Basic authentication reserves for tokens.
    #[error("password must not start with the API token prefix")]
    ReservedPrefix,
}

/// Policy for a NEW password (account creation, password change).
///
/// The `ryx_` prefix is refused because a Basic credential whose password starts
/// with it is routed to the API token authenticator and never reaches the local
/// password check: such a password would silently stop working.
///
/// The offline list of compromised passwords from SEC-09 is not part of this
/// crate; the API layer applies it before calling here.
pub fn validate_new_password(password: &str) -> Result<(), PasswordPolicyError> {
    if !is_usable_password(password) {
        return Err(PasswordPolicyError::Blank);
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordPolicyError::TooLong);
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PasswordPolicyError::TooShort);
    }
    if password.starts_with(API_TOKEN_PREFIX) {
        return Err(PasswordPolicyError::ReservedPrefix);
    }
    Ok(())
}

/// Invalid password hashing configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordConfigError {
    /// A parameter is below the OWASP floor.
    #[error("argon2id parameter `{0}` is below the accepted minimum")]
    BelowMinimum(&'static str),
    /// argon2 rejected the parameter combination.
    #[error("invalid argon2id parameters: {0}")]
    Invalid(String),
}

/// argon2id cost parameters for new hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordConfig {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Iterations.
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u32,
}

impl PasswordConfig {
    /// OWASP minimum memory, 19 MiB.
    pub const MIN_MEMORY_KIB: u32 = 19 * 1024;
    /// OWASP minimum iterations at the minimum memory.
    pub const MIN_ITERATIONS: u32 = 2;

    /// The recommended profile: 64 MiB, 3 iterations, 1 lane.
    #[must_use]
    pub const fn recommended() -> Self {
        Self {
            memory_kib: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }

    /// The lowest accepted profile (19 MiB, 2, 1). Useful where hashing speed matters
    /// more than margin, such as integration tests that still go through validation.
    #[must_use]
    pub const fn minimum() -> Self {
        Self {
            memory_kib: Self::MIN_MEMORY_KIB,
            iterations: Self::MIN_ITERATIONS,
            parallelism: 1,
        }
    }

    /// Checks the floor and that argon2 accepts the combination.
    pub fn validated(self) -> Result<Self, PasswordConfigError> {
        if self.memory_kib < Self::MIN_MEMORY_KIB {
            return Err(PasswordConfigError::BelowMinimum("memory_kib"));
        }
        if self.iterations < Self::MIN_ITERATIONS {
            return Err(PasswordConfigError::BelowMinimum("iterations"));
        }
        if self.parallelism < 1 {
            return Err(PasswordConfigError::BelowMinimum("parallelism"));
        }
        self.params()?;
        Ok(self)
    }

    fn params(self) -> Result<Params, PasswordConfigError> {
        Params::new(self.memory_kib, self.iterations, self.parallelism, None)
            .map_err(|error| PasswordConfigError::Invalid(error.to_string()))
    }
}

impl Default for PasswordConfig {
    fn default() -> Self {
        Self::recommended()
    }
}

/// Outcome of checking a password against a stored hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The password matches.
    Valid {
        /// The stored hash should be replaced by a fresh argon2id hash.
        needs_rehash: bool,
    },
    /// The password does not match, or the stored hash is unusable.
    Invalid,
}

/// Hashes and verifies local passwords under a [`BlockingGate`].
pub struct Passwords {
    params: Params,
    gate: Arc<BlockingGate>,
    dummy_hash: String,
}

impl std::fmt::Debug for Passwords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passwords")
            .field("m_kib", &self.params.m_cost())
            .field("t", &self.params.t_cost())
            .field("p", &self.params.p_cost())
            .finish_non_exhaustive()
    }
}

impl Passwords {
    /// Builds the service. Hashes one random password with the configured
    /// parameters to prepare the dummy hash, so the call takes one hash time
    /// (about 100 ms at the defaults): construct it at startup, not per request.
    pub fn new(
        config: PasswordConfig,
        gate: Arc<BlockingGate>,
    ) -> Result<Self, PasswordConfigError> {
        let config = config.validated()?;
        Self::build(config, gate)
    }

    /// Same as [`Passwords::new`] without the OWASP floor, for unit tests that
    /// would otherwise spend seconds hashing.
    #[cfg(test)]
    pub(crate) fn new_for_tests(
        config: PasswordConfig,
        gate: Arc<BlockingGate>,
    ) -> Result<Self, PasswordConfigError> {
        Self::build(config, gate)
    }

    fn build(config: PasswordConfig, gate: Arc<BlockingGate>) -> Result<Self, PasswordConfigError> {
        let params = config.params()?;
        let mut seed = [0_u8; 24];
        rand::fill(&mut seed);
        let seed_text = hex::encode(seed);
        let dummy_hash = hash_blocking(seed_text.as_bytes(), &params)
            .map_err(|error| PasswordConfigError::Invalid(error.to_string()))?;
        Ok(Self {
            params,
            gate,
            dummy_hash,
        })
    }

    /// Hashes a NEW password with argon2id. Blank passwords are refused before any work.
    pub async fn hash(&self, password: &Secret) -> Result<Secret, AuthError> {
        if !is_usable_password(password.expose()) {
            return Err(AuthError::InvalidCredentials);
        }
        let password = password.clone();
        let params = self.params.clone();
        let hashed = self
            .gate
            .run(GateClass::Password, move || {
                hash_blocking(password.expose().as_bytes(), &params)
            })
            .await?;
        hashed
            .map(Secret::new)
            .map_err(|error| backend_failure("password hashing", error.to_string()))
    }

    /// Verifies a password against a stored hash (argon2id PHC or imported bcrypt).
    ///
    /// Blank and oversized passwords are refused before any work and before the
    /// gate. An unparsable stored hash still pays one dummy verification so that
    /// "this account has no usable hash" is not visible in the response time.
    pub async fn verify(&self, password: &Secret, stored: &str) -> Result<Verification, AuthError> {
        if !is_usable_password(password.expose()) || password.expose().len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::InvalidCredentials);
        }
        let password = password.clone();
        let stored = stored.to_owned();
        let target = self.params.clone();
        let dummy = self.dummy_hash.clone();
        self.gate
            .run(GateClass::Password, move || {
                verify_blocking(password.expose().as_bytes(), &stored, &target, &dummy)
            })
            .await
    }

    /// Runs a verification that cannot succeed, for credentials that name no
    /// account. Same cost, same semaphore, no information in the result.
    pub async fn verify_dummy(&self, password: &Secret) -> Result<(), AuthError> {
        let password = password.clone();
        let target = self.params.clone();
        let dummy = self.dummy_hash.clone();
        self.gate
            .run(GateClass::Password, move || {
                // The result is discarded on purpose: only the cost matters.
                let _ = verify_argon2(password.expose().as_bytes(), &dummy, &target);
            })
            .await
    }
}

fn hash_blocking(password: &[u8], params: &Params) -> Result<String, argon2::password_hash::Error> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone())
        .hash_password(password)
        .map(|hash| hash.to_string())
}

fn verify_blocking(password: &[u8], stored: &str, target: &Params, dummy: &str) -> Verification {
    if BCRYPT_PREFIXES
        .iter()
        .any(|prefix| stored.starts_with(prefix))
    {
        return match bcrypt::verify(password, stored) {
            // Every imported bcrypt hash is rewritten as argon2id on success.
            Ok(true) => Verification::Valid { needs_rehash: true },
            // A malformed bcrypt hash or a NUL in the password is an error in the
            // crate; both mean "does not match".
            Ok(false) | Err(_) => Verification::Invalid,
        };
    }
    let Some(result) = verify_argon2(password, stored, target) else {
        // Unusable stored value: burn the same time a real check would.
        let _ = verify_argon2(password, dummy, target);
        tracing::warn!("stored password hash has an unsupported format");
        return Verification::Invalid;
    };
    result
}

/// `None` when `stored` is not an argon2id PHC string this crate accepts.
fn verify_argon2(password: &[u8], stored: &str, target: &Params) -> Option<Verification> {
    let parsed = PasswordHash::new(stored).ok()?;
    if parsed.algorithm != argon2::ARGON2ID_IDENT {
        // argon2i and argon2d are not what this service writes; refusing them keeps
        // a planted weaker variant from being accepted silently.
        return None;
    }
    let stored_params = Params::try_from(&parsed).ok()?;
    let matches = Argon2::default().verify_password(password, &parsed).is_ok();
    if !matches {
        return Some(Verification::Invalid);
    }
    let needs_rehash = parsed.version != Some(ARGON2_VERSION)
        || stored_params.m_cost() < target.m_cost()
        || stored_params.t_cost() < target.t_cost()
        || stored_params.p_cost() < target.p_cost();
    Some(Verification::Valid { needs_rehash })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn fast_config() -> PasswordConfig {
        PasswordConfig {
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        }
    }

    fn gate() -> Arc<BlockingGate> {
        Arc::new(BlockingGate::new(GateConfig::default()))
    }

    fn passwords() -> Passwords {
        Passwords::new_for_tests(fast_config(), gate()).unwrap()
    }

    fn bcrypt_hash(password: &str) -> String {
        bcrypt::hash(password, 4).unwrap()
    }

    #[tokio::test]
    async fn hashes_as_argon2id_phc_and_verifies() {
        let service = passwords();
        let hash = service
            .hash(&Secret::new("correct horse battery"))
            .await
            .unwrap();
        assert!(hash.expose().starts_with("$argon2id$v=19$m=8,t=1,p=1$"));
        let ok = service
            .verify(&Secret::new("correct horse battery"), hash.expose())
            .await
            .unwrap();
        assert_eq!(
            ok,
            Verification::Valid {
                needs_rehash: false
            }
        );
        let bad = service
            .verify(&Secret::new("correct horse batterz"), hash.expose())
            .await
            .unwrap();
        assert_eq!(bad, Verification::Invalid);
    }

    #[tokio::test]
    async fn salt_makes_two_hashes_of_one_password_differ() {
        let service = passwords();
        let secret = Secret::new("same password twice");
        let a = service.hash(&secret).await.unwrap();
        let b = service.hash(&secret).await.unwrap();
        assert_ne!(a.expose(), b.expose());
    }

    #[tokio::test]
    async fn bcrypt_hashes_verify_and_ask_for_rehash() {
        let service = passwords();
        let hash = bcrypt_hash("imported password");
        for prefix in ["$2a$", "$2b$", "$2y$"] {
            let variant = format!("{prefix}{}", &hash[4..]);
            let ok = service
                .verify(&Secret::new("imported password"), &variant)
                .await
                .unwrap();
            assert_eq!(ok, Verification::Valid { needs_rehash: true }, "{prefix}");
        }
        let bad = service.verify(&Secret::new("wrong"), &hash).await.unwrap();
        assert_eq!(bad, Verification::Invalid);
    }

    #[tokio::test]
    async fn must_not_trust_the_buggy_bcrypt_2x_marker() {
        let service = passwords();
        let hash = bcrypt_hash("imported password");
        let variant = format!("$2x${}", &hash[4..]);
        let result = service
            .verify(&Secret::new("imported password"), &variant)
            .await
            .unwrap();
        assert_eq!(result, Verification::Invalid);
    }

    #[tokio::test]
    async fn lower_parameters_than_configured_need_rehash() {
        let weak = passwords();
        let strong = Passwords::new_for_tests(
            PasswordConfig {
                memory_kib: 16,
                iterations: 2,
                parallelism: 1,
            },
            gate(),
        )
        .unwrap();
        let weak_hash = weak.hash(&Secret::new("upgrade me please")).await.unwrap();
        let outcome = strong
            .verify(&Secret::new("upgrade me please"), weak_hash.expose())
            .await
            .unwrap();
        assert_eq!(outcome, Verification::Valid { needs_rehash: true });
        let strong_hash = strong
            .hash(&Secret::new("upgrade me please"))
            .await
            .unwrap();
        let outcome = weak
            .verify(&Secret::new("upgrade me please"), strong_hash.expose())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            Verification::Valid {
                needs_rehash: false
            },
            "parameters are raised, never lowered"
        );
    }

    #[tokio::test]
    async fn must_not_hash_or_verify_blank_passwords() {
        // A gate with no free permit proves the gate is never reached: any attempt to
        // hash would fail with Overloaded instead of InvalidCredentials.
        let tight = Arc::new(BlockingGate::new(GateConfig {
            password: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            general: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            permit_wait: std::time::Duration::from_millis(1),
        }));
        let service = Passwords::new_for_tests(fast_config(), Arc::clone(&tight)).unwrap();
        let _held = tight.acquire(GateClass::Password).await.unwrap();
        for blank in ["", " ", "   \t\n", "\u{a0}\u{2003}"] {
            let verify = service
                .verify(&Secret::new(blank), "$argon2id$irrelevant")
                .await;
            assert!(
                matches!(verify, Err(AuthError::InvalidCredentials)),
                "{blank:?}"
            );
            let hash = service.hash(&Secret::new(blank)).await;
            assert!(
                matches!(hash, Err(AuthError::InvalidCredentials)),
                "{blank:?}"
            );
        }
    }

    #[tokio::test]
    async fn must_not_verify_oversized_passwords() {
        let service = passwords();
        let hash = service.hash(&Secret::new("a fine password")).await.unwrap();
        let huge = Secret::new("x".repeat(MAX_PASSWORD_BYTES + 1));
        let result = service.verify(&huge, hash.expose()).await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    }

    #[tokio::test]
    async fn must_not_accept_other_argon2_variants_or_garbage_hashes() {
        let service = passwords();
        let argon2i = "$argon2i$v=19$m=65536,t=1,p=1$c29tZXNhbHQAAAAAAAAAAA$+r0d29hqEB0yasKr55ZgICsQGSkl0v0kgwhd+U3wyRo";
        for stored in [argon2i, "", "plaintext", "$argon2id$", "$2a$not-a-hash"] {
            let result = service
                .verify(&Secret::new("password"), stored)
                .await
                .unwrap();
            assert_eq!(result, Verification::Invalid, "{stored}");
        }
    }

    #[tokio::test]
    async fn verify_dummy_pays_a_real_hash_under_the_gate() {
        let tight = Arc::new(BlockingGate::new(GateConfig {
            password: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            general: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            permit_wait: std::time::Duration::from_millis(1),
        }));
        let service = Passwords::new_for_tests(fast_config(), Arc::clone(&tight)).unwrap();
        service
            .verify_dummy(&Secret::new("anything"))
            .await
            .unwrap();
        let held = tight.acquire(GateClass::Password).await.unwrap();
        let blocked = service.verify_dummy(&Secret::new("anything")).await;
        assert!(
            matches!(blocked, Err(AuthError::Overloaded)),
            "the dummy check must compete for the same permits as a real one"
        );
        drop(held);
    }

    #[test]
    fn config_rejects_values_below_the_owasp_floor() {
        let low_memory = PasswordConfig {
            memory_kib: 1024,
            ..PasswordConfig::recommended()
        };
        assert_eq!(
            low_memory.validated(),
            Err(PasswordConfigError::BelowMinimum("memory_kib"))
        );
        let low_time = PasswordConfig {
            iterations: 1,
            ..PasswordConfig::recommended()
        };
        assert_eq!(
            low_time.validated(),
            Err(PasswordConfigError::BelowMinimum("iterations"))
        );
        let no_lanes = PasswordConfig {
            parallelism: 0,
            ..PasswordConfig::recommended()
        };
        assert_eq!(
            no_lanes.validated(),
            Err(PasswordConfigError::BelowMinimum("parallelism"))
        );
        let too_many_lanes = PasswordConfig {
            parallelism: 0x0100_0000,
            ..PasswordConfig::recommended()
        };
        assert!(matches!(
            too_many_lanes.validated(),
            Err(PasswordConfigError::Invalid(_))
        ));
        assert!(PasswordConfig::minimum().validated().is_ok());
        assert_eq!(PasswordConfig::default(), PasswordConfig::recommended());
    }

    #[tokio::test]
    async fn new_enforces_the_floor() {
        let result = Passwords::new(fast_config(), gate());
        assert!(matches!(result, Err(PasswordConfigError::BelowMinimum(_))));
        let service = Passwords::new(PasswordConfig::minimum(), gate()).unwrap();
        assert!(format!("{service:?}").contains("m_kib"));
    }

    #[test]
    fn new_password_policy() {
        assert_eq!(validate_new_password(""), Err(PasswordPolicyError::Blank));
        assert_eq!(
            validate_new_password("            "),
            Err(PasswordPolicyError::Blank)
        );
        assert_eq!(
            validate_new_password("short"),
            Err(PasswordPolicyError::TooShort)
        );
        assert_eq!(
            validate_new_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)),
            Err(PasswordPolicyError::TooLong)
        );
        assert_eq!(
            validate_new_password("ryx_looks-like-a-token"),
            Err(PasswordPolicyError::ReservedPrefix)
        );
        assert_eq!(validate_new_password("twelve chars!"), Ok(()));
        assert!(validate_new_password(&"x".repeat(MAX_PASSWORD_BYTES)).is_ok());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn blank_detection_matches_trim(text in "\\PC{0,40}") {
            prop_assert_eq!(is_usable_password(&text), !text.trim().is_empty());
        }

        #[test]
        fn policy_never_panics_and_agrees_with_length(text in "\\PC{0,80}") {
            let verdict = validate_new_password(&text);
            if verdict.is_ok() {
                prop_assert!(text.chars().count() >= MIN_PASSWORD_CHARS);
                prop_assert!(text.len() <= MAX_PASSWORD_BYTES);
                prop_assert!(!text.starts_with(API_TOKEN_PREFIX));
            }
        }
    }
}
