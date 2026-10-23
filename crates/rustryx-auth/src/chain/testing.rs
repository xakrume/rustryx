//! Shared harness for the chain tests: the real service over `MemStore`, with the
//! two external identity providers replaced by scripted doubles.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    AuthContext, AuthMethod, Credentials, IdpGroupMapping, IdpProvider, Principal, Role,
    RoleOrigin, RoleRef, User, UserSource,
};
use rustryx_core::ids::{IdpGroupMappingId, RoleId};
use rustryx_core::ports::{
    Authenticator, ExternalGroups, GroupResolver, IdpMappingRepo, LoginAttemptRepo, RoleRepo,
    UserRepo,
};
use rustryx_testkit::{FixedClock, MemStore, fixtures};
use serde_json::{Value, json};

use super::{AuthService, AuthServiceConfig, AuthServiceDeps, ExternalIdp};
use crate::basic::LocalPasswordAuthenticator;
use crate::groups::{JitConfig, JitProvisioner, ProviderPolicy, RoleMapper, RoleMapperConfig};
use crate::internal_jwt::{
    InternalJwtAuthenticator, InternalJwtConfig, InternalJwtService, IssueRequest, KeyRing,
    MemRevokedTokens,
};
use crate::login_guard::{GuardPolicy, LoginGuard};
use crate::passwords::{BlockingGate, GateConfig, PasswordConfig, Passwords};
use crate::tokens::TokenAuthenticator;

pub(crate) const ADMIN_PATH: &str = "/rustryx/admins";
pub(crate) const ADMIN_DN: &str = "cn=rustryx-admins,ou=groups,dc=example,dc=test";
pub(crate) const OIDC_ISSUER: &str = fixtures::OIDC_ISSUER;

type Handler = Box<dyn Fn(&Credentials) -> Result<Option<Principal>, AuthError> + Send + Sync>;

/// An authenticator whose answer is a function of the credentials, counting calls.
pub(crate) struct ScriptedAuthenticator {
    handler: Handler,
    calls: AtomicUsize,
}

impl ScriptedAuthenticator {
    fn new(handler: Handler) -> Arc<Self> {
        Arc::new(Self {
            handler,
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Authenticator for ScriptedAuthenticator {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.handler)(credentials)
    }
}

/// What a scripted resolver answers.
#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The groups already on the principal (what the OIDC resolver does).
    Echo,
    /// A fixed list.
    Groups(Vec<String>),
    /// A failed lookup.
    Fail,
    /// An answer labelled with the wrong provider.
    WrongProvider(Vec<String>),
}

pub(crate) struct ScriptedResolver {
    provider: IdpProvider,
    answer: Mutex<Answer>,
    calls: AtomicUsize,
}

impl ScriptedResolver {
    fn new(provider: IdpProvider, answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            provider,
            answer: Mutex::new(answer),
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = answer;
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl GroupResolver for ScriptedResolver {
    async fn external_groups(&self, principal: &Principal) -> Result<ExternalGroups, AuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self
            .answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match answer {
            Answer::Echo => Ok(ExternalGroups {
                provider: self.provider,
                groups: principal.external_groups.clone(),
            }),
            Answer::Groups(groups) => Ok(ExternalGroups {
                provider: self.provider,
                groups,
            }),
            Answer::Fail => Err(AuthError::GroupLookupFailed("scripted failure".into())),
            Answer::WrongProvider(groups) => Ok(ExternalGroups {
                provider: if self.provider == IdpProvider::Oidc {
                    IdpProvider::Ldap
                } else {
                    IdpProvider::Oidc
                },
                groups,
            }),
        }
    }
}

/// Knobs of the harness.
pub(crate) struct Options {
    pub(crate) mapper: RoleMapperConfig,
    pub(crate) jit: JitConfig,
    pub(crate) with_oidc: bool,
    pub(crate) with_ldap: bool,
    pub(crate) gate: Option<GateConfig>,
    /// Counters in a different store than the users, so one can fail without the other.
    pub(crate) separate_guard_store: bool,
    pub(crate) service: AuthServiceConfig,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mapper: RoleMapperConfig {
                oidc: ProviderPolicy {
                    admin_groups: vec![ADMIN_PATH.to_owned()],
                    ..ProviderPolicy::default()
                },
                ldap: ProviderPolicy {
                    admin_groups: vec![ADMIN_DN.to_owned()],
                    ..ProviderPolicy::default()
                },
                ..RoleMapperConfig::default()
            },
            jit: JitConfig::default(),
            with_oidc: true,
            with_ldap: true,
            gate: None,
            separate_guard_store: false,
            service: AuthServiceConfig::default(),
        }
    }
}

pub(crate) struct Harness {
    pub(crate) store: MemStore,
    pub(crate) guard_store: MemStore,
    pub(crate) clock: FixedClock,
    pub(crate) service: Arc<AuthService>,
    pub(crate) jwt: Arc<InternalJwtService>,
    pub(crate) passwords: Arc<Passwords>,
    pub(crate) mapper: Arc<RoleMapper>,
    pub(crate) jit: Arc<JitProvisioner>,
    pub(crate) gate: Arc<BlockingGate>,
    pub(crate) oidc_auth: Arc<ScriptedAuthenticator>,
    pub(crate) oidc_resolver: Arc<ScriptedResolver>,
    pub(crate) ldap_auth: Arc<ScriptedAuthenticator>,
    pub(crate) ldap_resolver: Arc<ScriptedResolver>,
    ldap_users: Arc<Mutex<HashMap<String, String>>>,
}

pub(crate) fn harness() -> Harness {
    harness_with(Options::default())
}

fn segment(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

/// A Keycloak-looking access token. The scripted OIDC authenticator reads the claims
/// from the payload; nothing here is signed, which is fine because the real
/// validation lives in the OIDC module, not in the chain.
pub(crate) fn oidc_token(username: &str, subject: &str, groups: &[&str], exp: i64) -> String {
    let header = json!({"alg": "RS256", "kid": "kc-1", "typ": "JWT"});
    let payload = json!({
        "iss": OIDC_ISSUER,
        "sub": subject,
        "preferred_username": username,
        "groups": groups,
        "exp": exp,
        "jti": format!("{username}-{subject}-{exp}"),
    });
    format!("{}.{}.c2ln", segment(&header), segment(&payload))
}

fn oidc_handler() -> Handler {
    Box::new(|credentials| {
        let Credentials::Bearer(token) = credentials else {
            return Ok(None);
        };
        let mut parts = token.expose().split('.');
        let (Some(header), Some(payload), Some(_)) = (parts.next(), parts.next(), parts.next())
        else {
            return Ok(None);
        };
        let Ok(header) = URL_SAFE_NO_PAD.decode(header) else {
            return Ok(None);
        };
        let header: Value = serde_json::from_slice(&header).unwrap_or(Value::Null);
        if header["alg"] != "RS256" {
            return Ok(None);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| AuthError::InvalidToken("payload"))?;
        let claims: Value =
            serde_json::from_slice(&payload).map_err(|_| AuthError::InvalidToken("claims"))?;
        if claims["iss"] != OIDC_ISSUER {
            return Err(AuthError::InvalidToken("wrong issuer"));
        }
        let groups = claims["groups"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|g| g.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Principal {
            user_id: None,
            username: claims["preferred_username"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            source: UserSource::Oidc,
            issuer: Some(OIDC_ISSUER.to_owned()),
            subject: claims["sub"].as_str().map(str::to_owned),
            roles: Vec::new(),
            external_groups: groups,
            groups_confirmed_at: None,
            token_scopes: None,
            token_id: None,
            auth_method: AuthMethod::OidcBearer,
        }))
    })
}

fn ldap_handler(users: Arc<Mutex<HashMap<String, String>>>) -> Handler {
    Box::new(move |credentials| {
        let Credentials::Basic { username, password } = credentials else {
            return Ok(None);
        };
        let directory = users.lock().unwrap_or_else(PoisonError::into_inner);
        match directory.get(username) {
            Some(expected) if expected == password.expose() => Ok(Some(Principal {
                user_id: None,
                username: username.clone(),
                source: UserSource::Ldap,
                issuer: None,
                subject: None,
                roles: Vec::new(),
                external_groups: Vec::new(),
                groups_confirmed_at: None,
                token_scopes: None,
                token_id: None,
                auth_method: AuthMethod::BasicLdap,
            })),
            // A failed bind, whether the user exists or not.
            _ => Err(AuthError::InvalidCredentials),
        }
    })
}

pub(crate) fn weak_password_config() -> PasswordConfig {
    PasswordConfig {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    }
}

pub(crate) fn harness_with(options: Options) -> Harness {
    let store = MemStore::with_builtin_roles();
    let guard_store = if options.separate_guard_store {
        MemStore::new()
    } else {
        store.clone()
    };
    let clock = FixedClock::default();
    let gate = Arc::new(BlockingGate::new(options.gate.unwrap_or_default()));
    let passwords =
        Arc::new(Passwords::new_for_tests(weak_password_config(), Arc::clone(&gate)).unwrap());
    let revoked = Arc::new(MemRevokedTokens::new());
    let jwt = Arc::new(InternalJwtService::new(
        KeyRing::from_seed("k1", &[7; 32], &[]).unwrap(),
        InternalJwtConfig::default(),
        Arc::new(clock.clone()),
        Arc::new(store.clone()),
        revoked.clone(),
    ));
    let mapper = Arc::new(RoleMapper::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        options.mapper,
    ));
    let jit = Arc::new(JitProvisioner::new(
        Arc::new(store.clone()),
        Arc::new(clock.clone()),
        options.jit,
    ));
    let ldap_users = Arc::new(Mutex::new(HashMap::new()));
    let oidc_auth = ScriptedAuthenticator::new(oidc_handler());
    let oidc_resolver = ScriptedResolver::new(IdpProvider::Oidc, Answer::Echo);
    let ldap_auth = ScriptedAuthenticator::new(ldap_handler(Arc::clone(&ldap_users)));
    let ldap_resolver = ScriptedResolver::new(IdpProvider::Ldap, Answer::Groups(Vec::new()));
    let guard_repo: Arc<dyn LoginAttemptRepo> = Arc::new(guard_store.clone());
    let service = AuthService::new(
        AuthServiceDeps {
            users: Arc::new(store.clone()),
            clock: Arc::new(clock.clone()),
            local_basic: Arc::new(LocalPasswordAuthenticator::new(
                Arc::new(store.clone()),
                Arc::clone(&passwords),
            )),
            api_token: Arc::new(TokenAuthenticator::new(
                Arc::new(store.clone()),
                Arc::new(store.clone()),
                Arc::new(clock.clone()),
            )),
            internal_jwt: Arc::new(InternalJwtAuthenticator::new(Arc::clone(&jwt))),
            oidc: options.with_oidc.then(|| ExternalIdp {
                authenticator: oidc_auth.clone(),
                resolver: oidc_resolver.clone(),
            }),
            ldap: options.with_ldap.then(|| ExternalIdp {
                authenticator: ldap_auth.clone(),
                resolver: ldap_resolver.clone(),
            }),
            guard: Arc::new(LoginGuard::new(
                guard_repo,
                Arc::new(clock.clone()),
                GuardPolicy::default(),
            )),
            passwords: Arc::clone(&passwords),
            mapper: Arc::clone(&mapper),
            jit: Arc::clone(&jit),
        },
        options.service,
    );
    Harness {
        store,
        guard_store,
        clock,
        service: Arc::new(service),
        jwt,
        passwords,
        mapper,
        jit,
        gate,
        oidc_auth,
        oidc_resolver,
        ldap_auth,
        ldap_resolver,
        ldap_users,
    }
}

impl Harness {
    pub(crate) fn add_ldap_user(&self, name: &str, password: &str) {
        self.ldap_users
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), password.to_owned());
    }

    pub(crate) async fn add_local_user(&self, name: &str, password: &str) -> User {
        let mut new = fixtures::user(name, UserSource::Local);
        new.password_hash = Some(self.passwords.hash(&Secret::new(password)).await.unwrap());
        UserRepo::create(&self.store, &new).await.unwrap()
    }

    pub(crate) async fn add_role(&self, name: &str) -> RoleRef {
        let role = Role {
            id: RoleId::generate(),
            name: name.to_owned(),
            description: String::new(),
            builtin: false,
            privilege_ids: Vec::new(),
            nested_role_ids: Vec::new(),
        };
        RoleRepo::create(&self.store, &role).await.unwrap();
        RoleRef {
            id: role.id,
            name: role.name,
        }
    }

    pub(crate) async fn map_group(&self, provider: IdpProvider, group: &str, role: &RoleRef) {
        let mapping = IdpGroupMapping {
            id: IdpGroupMappingId::generate(),
            provider,
            external_group: group.to_owned(),
            role_id: role.id,
            priority: 10,
        };
        IdpMappingRepo::upsert(&self.store, &mapping).await.unwrap();
        self.mapper.invalidate();
    }

    pub(crate) async fn grant_manual(&self, user: &User, role: &RoleRef) {
        UserRepo::replace_roles_for_origin(&self.store, user.id, RoleOrigin::Manual, &[role.id])
            .await
            .unwrap();
    }

    pub(crate) async fn authenticate(
        &self,
        credentials: &Credentials,
    ) -> Result<Principal, AuthError> {
        self.authenticate_from(credentials, None).await
    }

    pub(crate) async fn authenticate_from(
        &self,
        credentials: &Credentials,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<Principal, AuthError> {
        let context = AuthContext {
            client_ip,
            request_id: None,
        };
        let principal = self.service.authenticate(credentials, &context).await?;
        Ok(principal.expect("the chain never returns Ok(None)"))
    }

    pub(crate) async fn basic(&self, user: &str, password: &str) -> Result<Principal, AuthError> {
        self.authenticate(&basic(user, password)).await
    }

    pub(crate) async fn bearer(&self, token: &str) -> Result<Principal, AuthError> {
        self.authenticate(&Credentials::Bearer(Secret::new(token)))
            .await
    }

    pub(crate) fn clock_now(&self) -> time::OffsetDateTime {
        rustryx_core::ports::Clock::now(&self.clock)
    }

    /// Token expiry `seconds` from the harness clock.
    pub(crate) fn exp_in(&self, seconds: i64) -> i64 {
        self.clock.now_seconds() + seconds
    }

    pub(crate) fn issue_jwt(&self, user: &User, ttl: Duration) -> String {
        self.jwt
            .issue(&IssueRequest {
                user_id: user.id,
                ttl,
                groups: None,
            })
            .unwrap()
            .token
            .expose()
            .to_owned()
    }
}

pub(crate) fn basic(user: &str, password: &str) -> Credentials {
    Credentials::Basic {
        username: user.to_owned(),
        password: Secret::new(password),
    }
}

pub(crate) fn names(principal: &Principal) -> Vec<&str> {
    let mut names: Vec<&str> = principal.roles.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    names
}

trait ClockSeconds {
    fn now_seconds(&self) -> i64;
}

impl ClockSeconds for FixedClock {
    fn now_seconds(&self) -> i64 {
        rustryx_core::ports::Clock::now(self).unix_timestamp()
    }
}

/// Fresh dependencies equal to the harness's own, for tests that swap one part.
pub(crate) fn deps_for(h: &Harness) -> AuthServiceDeps {
    AuthServiceDeps {
        users: Arc::new(h.store.clone()),
        clock: Arc::new(h.clock.clone()),
        local_basic: Arc::new(LocalPasswordAuthenticator::new(
            Arc::new(h.store.clone()),
            Arc::clone(&h.passwords),
        )),
        api_token: Arc::new(TokenAuthenticator::new(
            Arc::new(h.store.clone()),
            Arc::new(h.store.clone()),
            Arc::new(h.clock.clone()),
        )),
        internal_jwt: Arc::new(InternalJwtAuthenticator::new(Arc::clone(&h.jwt))),
        oidc: Some(ExternalIdp {
            authenticator: h.oidc_auth.clone(),
            resolver: h.oidc_resolver.clone(),
        }),
        ldap: Some(ExternalIdp {
            authenticator: h.ldap_auth.clone(),
            resolver: h.ldap_resolver.clone(),
        }),
        guard: Arc::new(LoginGuard::new(
            Arc::new(h.guard_store.clone()),
            Arc::new(h.clock.clone()),
            GuardPolicy::default(),
        )),
        passwords: Arc::clone(&h.passwords),
        mapper: Arc::clone(&h.mapper),
        jit: Arc::clone(&h.jit),
    }
}
