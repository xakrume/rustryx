//! The harness authenticator: users with a password and an API token, scoped tokens and the
//! short lived bearer tokens of the OCI token endpoint.
//!
//! It is deliberately small. It parses `Authorization` the way the real HTTP layer does (no
//! header is anonymous, an unusable header is never anonymous, Basic with a `ryx_` password is
//! an API token whatever the user name says, because `twine` sends `__token__`), and it takes
//! the roles of a user from the store at request time, so a role changed after the first
//! request is seen by the next one. Everything about what a principal may DO is `Rbac`'s.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, URL_SAFE_NO_PAD};
use http::HeaderMap;
use http::header::AUTHORIZATION;
use rustryx_core::identity::{API_TOKEN_PREFIX, AuthMethod, Principal, UserSource};
use rustryx_core::ids::UserId;
use rustryx_core::ports::UserRepo;

/// Prefix of the short lived bearer tokens issued by `/v2/token`. Distinct from `ryx_` so a
/// test can tell an API token from a registry token at a glance.
pub(crate) const OCI_TOKEN_PREFIX: &str = "v2t_";

/// Entropy of every generated secret, in bytes.
const SECRET_BYTES: usize = 32;

/// Many clients drop the `=` padding of the Basic payload, so padding is optional (as in the
/// real HTTP layer).
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Longest `Authorization` value taken seriously, like the real layer: anything near it is abuse.
const MAX_AUTHORIZATION_BYTES: usize = 16 * 1024;

/// What the `Authorization` header of a request says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Presented {
    /// No header: anonymous.
    Nothing,
    /// `Basic user:password`.
    Basic { username: String, password: String },
    /// `Bearer token`.
    Bearer(String),
    /// Present but unusable (unknown scheme, bad base64, empty password, duplicated header).
    /// Never anonymous: a client that tried to authenticate must not be downgraded.
    Malformed,
}

/// Reads the `Authorization` header of a request.
pub(crate) fn parse_authorization(headers: &HeaderMap) -> Presented {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(first) = values.next() else {
        return Presented::Nothing;
    };
    if values.next().is_some() || first.len() > MAX_AUTHORIZATION_BYTES {
        return Presented::Malformed;
    }
    let Ok(text) = first.to_str() else {
        return Presented::Malformed;
    };
    let Some((scheme, rest)) = text.split_once(' ') else {
        return Presented::Malformed;
    };
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("basic") {
        let Ok(decoded) = LENIENT_BASE64.decode(rest) else {
            return Presented::Malformed;
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return Presented::Malformed;
        };
        match decoded.split_once(':') {
            Some((user, password)) if !password.is_empty() && !password.contains('\0') => {
                Presented::Basic {
                    username: user.to_owned(),
                    password: password.to_owned(),
                }
            }
            _ => Presented::Malformed,
        }
    } else if scheme.eq_ignore_ascii_case("bearer") && !rest.is_empty() {
        Presented::Bearer(rest.to_owned())
    } else {
        Presented::Malformed
    }
}

/// The verdict of the authenticator for one request.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// No credentials were presented.
    Anonymous,
    /// Credentials were valid.
    Principal(Box<Principal>),
    /// Credentials were presented and are wrong or expired: 401, never anonymous.
    Rejected,
    /// The store that holds the roles failed: 503, never "no roles".
    Unavailable,
}

#[derive(Clone)]
enum Owner {
    User(String),
    /// A token issued to an unauthenticated caller by the OCI token endpoint.
    Anonymous,
}

#[derive(Clone)]
struct TokenEntry {
    owner: Owner,
    method: AuthMethod,
    scopes: Option<Vec<String>>,
    expires: Option<Instant>,
}

struct Account {
    user_id: UserId,
    password: String,
}

#[derive(Default)]
struct BookState {
    accounts: HashMap<String, Account>,
    tokens: HashMap<String, TokenEntry>,
}

/// Users, passwords and tokens of one [`TestServer`](crate::TestServer).
pub(crate) struct CredentialBook {
    users: Arc<dyn UserRepo>,
    state: Mutex<BookState>,
    oci_ttl: Duration,
}

/// A fresh secret: `prefix` and 32 random bytes in base64url, the shape of a real API token.
pub(crate) fn generate_secret(prefix: &str) -> String {
    let mut bytes = [0_u8; SECRET_BYTES];
    rand::fill(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

impl CredentialBook {
    pub(crate) fn new(users: Arc<dyn UserRepo>, oci_ttl: Duration) -> Self {
        Self {
            users,
            state: Mutex::new(BookState::default()),
            oci_ttl,
        }
    }

    fn lock(&self) -> MutexGuard<'_, BookState> {
        // A poisoned lock only means another test thread panicked; the maps stay consistent
        // because every critical section is a single insert or lookup.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers an account with its password and its long lived API token.
    pub(crate) fn register_user(
        &self,
        username: &str,
        user_id: UserId,
        password: &str,
        token: &str,
    ) {
        let mut state = self.lock();
        state.accounts.insert(
            username.to_owned(),
            Account {
                user_id,
                password: password.to_owned(),
            },
        );
        state.tokens.insert(
            token.to_owned(),
            TokenEntry {
                owner: Owner::User(username.to_owned()),
                method: AuthMethod::ApiToken,
                scopes: None,
                expires: None,
            },
        );
    }

    /// Issues an extra API token for `username` restricted to `scopes`. `None` when the
    /// account does not exist.
    pub(crate) fn issue_scoped_token(&self, username: &str, scopes: &[String]) -> Option<String> {
        let mut state = self.lock();
        if !state.accounts.contains_key(username) {
            return None;
        }
        let token = generate_secret(API_TOKEN_PREFIX);
        state.tokens.insert(
            token.clone(),
            TokenEntry {
                owner: Owner::User(username.to_owned()),
                method: AuthMethod::ApiToken,
                scopes: Some(scopes.to_vec()),
                expires: None,
            },
        );
        Some(token)
    }

    /// Issues a short lived registry token for `principal` (anonymous or a user) and returns
    /// it with its lifetime in seconds. The scopes of an API token carry over, so a registry
    /// token never widens the credential it was issued for.
    pub(crate) fn issue_oci_token(&self, principal: &Principal) -> (String, u64) {
        let owner = if principal.is_anonymous() {
            Owner::Anonymous
        } else {
            Owner::User(principal.username.clone())
        };
        let token = generate_secret(OCI_TOKEN_PREFIX);
        self.lock().tokens.insert(
            token.clone(),
            TokenEntry {
                owner,
                method: AuthMethod::InternalJwt,
                scopes: principal.token_scopes.clone(),
                expires: Some(Instant::now() + self.oci_ttl),
            },
        );
        (token, self.oci_ttl.as_secs().max(1))
    }

    /// Decides who the caller is.
    pub(crate) async fn authenticate(&self, headers: &HeaderMap) -> Outcome {
        match parse_authorization(headers) {
            Presented::Nothing => Outcome::Anonymous,
            Presented::Malformed => Outcome::Rejected,
            Presented::Basic { username, password } => self.basic(&username, &password).await,
            Presented::Bearer(token) => self.by_token(&token).await,
        }
    }

    /// Checks a user name and a password (or an API token in the password position).
    pub(crate) async fn basic(&self, username: &str, password: &str) -> Outcome {
        if password.starts_with(API_TOKEN_PREFIX) || password.starts_with(OCI_TOKEN_PREFIX) {
            // The user name is ignored on purpose: `twine` sends `__token__`, and the token
            // alone identifies its owner.
            return self.by_token(password).await;
        }
        let user_id = {
            let state = self.lock();
            state
                .accounts
                .get(username)
                .filter(|account| account.password == password)
                .map(|account| account.user_id)
        };
        match user_id {
            Some(id) => {
                self.principal(username, id, AuthMethod::BasicLocal, None)
                    .await
            }
            None => Outcome::Rejected,
        }
    }

    async fn by_token(&self, token: &str) -> Outcome {
        let found = {
            let state = self.lock();
            state.tokens.get(token).cloned().map(|entry| {
                let id = match &entry.owner {
                    Owner::User(name) => state.accounts.get(name).map(|a| a.user_id),
                    Owner::Anonymous => None,
                };
                (entry, id)
            })
        };
        let Some((entry, user_id)) = found else {
            return Outcome::Rejected;
        };
        if entry.expires.is_some_and(|at| Instant::now() >= at) {
            return Outcome::Rejected;
        }
        match (entry.owner, user_id) {
            (Owner::Anonymous, _) => Outcome::Principal(Box::new(Principal::anonymous())),
            (Owner::User(name), Some(id)) => {
                self.principal(&name, id, entry.method, entry.scopes).await
            }
            (Owner::User(_), None) => Outcome::Rejected,
        }
    }

    async fn principal(
        &self,
        username: &str,
        user_id: UserId,
        method: AuthMethod,
        scopes: Option<Vec<String>>,
    ) -> Outcome {
        let roles = match self.users.roles_of(user_id).await {
            Ok(roles) => roles,
            Err(error) => {
                tracing::warn!(%error, "harness authenticator could not read the roles of a user");
                return Outcome::Unavailable;
            }
        };
        let mut principal = Principal::new(username, UserSource::Local, method);
        principal.user_id = Some(user_id);
        principal.roles = roles.into_iter().map(|(role, _origin)| role).collect();
        principal.token_scopes = scopes;
        Outcome::Principal(Box::new(principal))
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use rustryx_core::identity::UserSource;
    use rustryx_core::ports::UserRepo;
    use rustryx_testkit::{MemStore, fixtures};

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        map
    }

    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    #[test]
    fn parses_the_forms_clients_send() {
        assert_eq!(parse_authorization(&HeaderMap::new()), Presented::Nothing);
        assert_eq!(
            parse_authorization(&headers(&basic("alice", "pw"))),
            Presented::Basic {
                username: "alice".into(),
                password: "pw".into()
            }
        );
        assert_eq!(
            parse_authorization(&headers("bearer tok")),
            Presented::Bearer("tok".into())
        );
        // Padding is optional in the wild.
        assert_eq!(
            parse_authorization(&headers("Basic YWxpY2U6cHc")),
            Presented::Basic {
                username: "alice".into(),
                password: "pw".into()
            }
        );
    }

    #[test]
    fn an_unusable_header_is_never_anonymous() {
        for bad in [
            "Digest abc",
            "Basic !!!",
            "Bearer ",
            "Basic YWxpY2U6",
            "justonetoken",
        ] {
            assert_eq!(
                parse_authorization(&headers(bad)),
                Presented::Malformed,
                "{bad}"
            );
        }
        let mut twice = headers("Bearer a");
        twice.append(AUTHORIZATION, HeaderValue::from_static("Bearer b"));
        assert_eq!(parse_authorization(&twice), Presented::Malformed);
    }

    #[test]
    fn generated_secrets_look_like_real_tokens_and_differ() {
        let a = generate_secret(API_TOKEN_PREFIX);
        let b = generate_secret(API_TOKEN_PREFIX);
        assert!(a.starts_with("ryx_"));
        assert_eq!(a.len(), 4 + 43);
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn registry_tokens_expire() {
        let book = CredentialBook::new(Arc::new(MemStore::new()), Duration::from_millis(20));
        let (token, ttl) = book.issue_oci_token(&Principal::anonymous());
        assert_eq!(ttl, 1);
        assert!(matches!(
            book.authenticate(&headers(&format!("Bearer {token}"))).await,
            Outcome::Principal(p) if p.is_anonymous()
        ));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(matches!(
            book.authenticate(&headers(&format!("Bearer {token}")))
                .await,
            Outcome::Rejected
        ));
    }

    #[tokio::test]
    async fn a_registry_token_keeps_the_scopes_of_the_api_token() {
        let store = MemStore::new();
        let created = store
            .create(&fixtures::user("alice", UserSource::Local))
            .await
            .unwrap();
        let book = CredentialBook::new(Arc::new(store), Duration::from_secs(60));
        book.register_user(
            "alice",
            created.id,
            "pw",
            &generate_secret(API_TOKEN_PREFIX),
        );
        let scoped = book
            .issue_scoped_token("alice", &["repo:maven-hosted:read".to_owned()])
            .unwrap();
        let Outcome::Principal(principal) = book
            .authenticate(&headers(&format!("Bearer {scoped}")))
            .await
        else {
            panic!("the scoped API token was not accepted");
        };
        assert_eq!(
            principal.token_scopes.as_deref(),
            Some(["repo:maven-hosted:read".to_owned()].as_slice())
        );
        let (registry, _) = book.issue_oci_token(&principal);
        let Outcome::Principal(again) = book
            .authenticate(&headers(&format!("Bearer {registry}")))
            .await
        else {
            panic!("the registry token was not accepted");
        };
        assert_eq!(again.token_scopes, principal.token_scopes);
        assert!(!again.is_anonymous());
    }
}
