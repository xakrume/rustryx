//! Fakes and a harness for the tests of the cookie session and the native endpoints.
//!
//! [`FakeAuth`] plays both the token service and the authentication chain with the semantics of
//! the real ones that the HTTP layer depends on: a token is refused after its `exp`, after it
//! was revoked, and an SSO token whose group snapshot is older than five minutes is refused
//! (the chain's rule). That is enough to drive the whole cookie path end to end in process; the
//! real services meet the real router in the server's end-to-end test.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use http::{HeaderMap, Method, Request, StatusCode};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    AuthContext, AuthMethod, Credentials, Principal, RoleRef, UserSource,
};
use rustryx_core::ids::{ApiTokenId, RoleId, UserId};
use rustryx_core::ports::{
    AuditSink, Authenticator, Clock, RbacChecker, RoleRepo, TokenRepo, UserRepo,
};
use rustryx_core::secret::Secret;
use rustryx_testkit::{FixedClock, MemAuditSink, MemStore};
use time::OffsetDateTime;

use crate::ports::{
    GroupSnapshot, IssuedSession, OidcCallback, OidcCompletion, OidcLogin, OidcLoginPort,
    OidcStart, SessionIssuer, SessionRequest, SsoRefreshRequest, SsoRefreshed, SsoSessionRefresher,
};
use crate::session::claims::{forge, peek};
use crate::session::{CsrfKey, SessionManager, SessionSettings};
use crate::test_support::{Reply, call};
use crate::{
    ApiSettings, AppState, AuthApiDeps, AuthApiSettings, Lifecycle, MetricsAccess,
    NativeAuthModule, ProbeReport, RouteCatalog, RouterBuilder,
};

/// An account the fake chain knows.
#[derive(Clone)]
pub(crate) struct Account {
    pub user_id: UserId,
    pub password: String,
    pub source: UserSource,
    pub roles: Vec<RoleRef>,
}

/// Token service and chain in one.
pub(crate) struct FakeAuth {
    clock: FixedClock,
    counter: AtomicU64,
    accounts: Mutex<HashMap<String, Account>>,
    revoked: Mutex<HashSet<String>>,
    /// `tid`s of every token issued, oldest first.
    issued: Mutex<Vec<String>>,
    pub(crate) fail_issue: Mutex<bool>,
    pub(crate) fail_revoke: Mutex<bool>,
    pub(crate) chain_unavailable: Mutex<bool>,
}

impl FakeAuth {
    pub(crate) fn new(clock: FixedClock) -> Arc<Self> {
        Arc::new(Self {
            clock,
            counter: AtomicU64::new(0),
            accounts: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashSet::new()),
            issued: Mutex::new(Vec::new()),
            fail_issue: Mutex::new(false),
            fail_revoke: Mutex::new(false),
            chain_unavailable: Mutex::new(false),
        })
    }

    pub(crate) fn add(&self, username: &str, account: Account) {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(username.to_owned(), account);
    }

    /// Replaces the roles of an account of the fake chain.
    pub(crate) fn set_roles(&self, username: &str, roles: Vec<RoleRef>) {
        if let Some(account) = self
            .accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(username)
        {
            account.roles = roles;
        }
    }

    pub(crate) fn issued(&self) -> Vec<String> {
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn revoke_for_test(&self, tid: &str) {
        self.revoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tid.to_owned());
    }

    pub(crate) fn is_revoked(&self, tid: &str) -> bool {
        self.revoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(tid)
    }

    fn account_of(&self, user_id: UserId) -> Option<(String, Account)> {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(_, account)| account.user_id == user_id)
            .map(|(name, account)| (name.clone(), account.clone()))
    }

    fn flag(flag: &Mutex<bool>) -> bool {
        *flag.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl SessionIssuer for FakeAuth {
    fn issue(&self, request: &SessionRequest) -> Result<IssuedSession, AuthError> {
        if Self::flag(&self.fail_issue) {
            return Err(AuthError::Backend {
                context: "issue session",
                source: "signing failed".into(),
            });
        }
        let now = self.clock.now();
        let tid = format!("tid-{}", self.counter.fetch_add(1, Ordering::SeqCst) + 1);
        let iat = now.unix_timestamp();
        let ttl = i64::try_from(request.ttl.as_secs() + u64::from(request.ttl.subsec_nanos() > 0))
            .unwrap_or(i64::MAX);
        let exp = iat + ttl;
        let mut claims = serde_json::json!({
            "sub": request.user_id.to_string(), "tid": tid, "iat": iat, "exp": exp,
        });
        if let (Some(snapshot), Some(object)) = (&request.groups, claims.as_object_mut()) {
            object.insert("grp".into(), serde_json::json!(snapshot.groups));
            object.insert(
                "gat".into(),
                serde_json::json!(snapshot.confirmed_at.unix_timestamp()),
            );
        }
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tid.clone());
        Ok(IssuedSession::new(
            Secret::new(forge(&claims)),
            tid,
            OffsetDateTime::from_unix_timestamp(iat).unwrap_or(now),
            OffsetDateTime::from_unix_timestamp(exp).unwrap_or(now),
        ))
    }

    async fn revoke(
        &self,
        _user_id: UserId,
        tid: &str,
        _until: OffsetDateTime,
    ) -> Result<(), AuthError> {
        if Self::flag(&self.fail_revoke) {
            return Err(AuthError::GuardUnavailable);
        }
        self.revoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tid.to_owned());
        Ok(())
    }
}

#[async_trait]
impl Authenticator for FakeAuth {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        if Self::flag(&self.chain_unavailable) {
            return Err(AuthError::ProviderUnavailable("database".into()));
        }
        match credentials {
            Credentials::Anonymous => Ok(Some(Principal::anonymous())),
            Credentials::Basic { username, password } => {
                let accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
                match accounts.get(username) {
                    Some(account) if account.password == password.expose() => {
                        let method = if account.source == UserSource::Ldap {
                            AuthMethod::BasicLdap
                        } else {
                            AuthMethod::BasicLocal
                        };
                        let mut principal =
                            Principal::new(username.clone(), account.source, method);
                        principal.user_id = Some(account.user_id);
                        principal.roles.clone_from(&account.roles);
                        Ok(Some(principal))
                    }
                    _ => Err(AuthError::InvalidCredentials),
                }
            }
            Credentials::Bearer(token) => self.bearer(token.expose()),
        }
    }
}

impl FakeAuth {
    fn bearer(&self, token: &str) -> Result<Option<Principal>, AuthError> {
        if let Some(name) = token.strip_prefix("ryx_") {
            let (username, account) = self
                .accounts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|(user, _)| user.as_str() == name)
                .map(|(user, account)| (user.clone(), account.clone()))
                .ok_or(AuthError::InvalidToken("unknown token"))?;
            let mut principal = Principal::new(username, account.source, AuthMethod::ApiToken);
            principal.user_id = Some(account.user_id);
            principal.roles = account.roles;
            principal.token_id = Some(ApiTokenId::from_uuid(account.user_id.as_uuid()));
            principal.token_scopes = Some(vec!["repo:*:read".to_owned()]);
            return Ok(Some(principal));
        }
        let Some(claims) = peek(token) else {
            return Err(AuthError::InvalidToken("malformed"));
        };
        let now = self.clock.now();
        if now >= claims.expires_at {
            return Err(AuthError::TokenExpired);
        }
        if self.is_revoked(&claims.tid) {
            return Err(AuthError::TokenRevoked);
        }
        let (username, account) = self
            .account_of(claims.user_id)
            .ok_or(AuthError::InvalidToken("unknown subject"))?;
        let mut principal = Principal::new(username, account.source, AuthMethod::InternalJwt);
        principal.user_id = Some(account.user_id);
        principal.roles = account.roles;
        principal.expires_at = Some(claims.expires_at);
        if let Some(snapshot) = claims.groups {
            // The chain's rule: a snapshot older than five minutes grants nothing.
            if now - snapshot.confirmed_at > time::Duration::minutes(5) {
                return Err(AuthError::GroupLookupFailed("snapshot too old".into()));
            }
            principal.external_groups = snapshot.groups;
            principal.groups_confirmed_at = Some(snapshot.confirmed_at);
        }
        Ok(Some(principal))
    }
}

/// What the SSO adapter saw of a callback: code, state, error and the cookie header.
pub(crate) type CallbackSeen = (Option<String>, Option<String>, Option<String>, String);

/// An SSO adapter that logs `sso-user` in with fixed groups, and records what it was asked.
pub(crate) struct FakeOidc {
    pub clock: FixedClock,
    pub user_id: UserId,
    pub groups: Vec<String>,
    pub available: Mutex<bool>,
    pub next_error: Mutex<Option<fn() -> AuthError>>,
    pub return_to: Mutex<String>,
    pub carrier: Option<String>,
    pub seen: Mutex<Vec<CallbackSeen>>,
}

impl FakeOidc {
    pub(crate) fn new(clock: FixedClock, user_id: UserId, groups: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            clock,
            user_id,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            available: Mutex::new(true),
            next_error: Mutex::new(None),
            return_to: Mutex::new("/".to_owned()),
            carrier: None,
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl OidcLoginPort for FakeOidc {
    fn available(&self) -> bool {
        *self
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn start(&self, return_to: &str) -> Result<OidcStart, AuthError> {
        if !self.available() {
            return Err(AuthError::ProviderUnavailable("keycloak".into()));
        }
        *self
            .return_to
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = return_to.to_owned();
        Ok(OidcStart::new(
            "https://sso.example.test/auth?state=abc".to_owned(),
            vec!["rustryx_oidc_flow=sealed; Max-Age=600; Path=/api/v1/auth/oidc/callback; HttpOnly; Secure; SameSite=Lax".to_owned()],
        ))
    }

    async fn complete(&self, callback: &OidcCallback<'_>) -> Result<OidcCompletion, AuthError> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                callback.code.map(str::to_owned),
                callback.state.map(str::to_owned),
                callback.error.map(str::to_owned),
                callback.cookie_header.to_owned(),
            ));
        let next = self
            .next_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(make) = next {
            return Err(make());
        }
        let snapshot = GroupSnapshot {
            groups: self.groups.clone(),
            confirmed_at: self.clock.now(),
        };
        let mut login = OidcLogin::new(self.user_id, snapshot);
        if let Some(carrier) = &self.carrier {
            login = login.with_carrier(carrier.clone());
        }
        Ok(OidcCompletion::new(
            login,
            self.return_to
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            self.clear_cookies(),
        ))
    }

    fn clear_cookies(&self) -> Vec<String> {
        vec!["rustryx_oidc_flow=; Max-Age=0; Path=/api/v1/auth/oidc/callback; HttpOnly; Secure; SameSite=Lax".to_owned()]
    }
}

/// A refresher that answers from a script.
pub(crate) struct FakeRefresher {
    pub clock: FixedClock,
    pub groups: Mutex<Vec<String>>,
    pub fail: Mutex<Option<fn() -> AuthError>>,
    pub calls: AtomicU64,
    pub rotate: bool,
}

impl FakeRefresher {
    pub(crate) fn new(clock: FixedClock, groups: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            clock,
            groups: Mutex::new(groups.iter().map(|g| (*g).to_owned()).collect()),
            fail: Mutex::new(None),
            calls: AtomicU64::new(0),
            rotate: false,
        })
    }
}

#[async_trait]
impl SsoSessionRefresher for FakeRefresher {
    async fn refresh(&self, request: &SsoRefreshRequest<'_>) -> Result<SsoRefreshed, AuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let fail = *self.fail.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(make) = fail {
            return Err(make());
        }
        assert_ne!(request.carrier, "");
        let snapshot = GroupSnapshot {
            groups: self
                .groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            confirmed_at: self.clock.now(),
        };
        let mut refreshed = SsoRefreshed::new(snapshot);
        if self.rotate {
            refreshed = refreshed.with_carrier("rotated-carrier".to_owned());
        }
        Ok(refreshed)
    }
}

/// The service origin the harness uses.
pub(crate) const ORIGIN: &str = "https://repo.example.test";

/// Everything a session test needs.
pub(crate) struct Env {
    pub clock: FixedClock,
    pub auth: Arc<FakeAuth>,
    pub store: Arc<MemStore>,
    pub audit: MemAuditSink,
    pub state: AppState,
    pub router: Router,
    pub catalog: RouteCatalog,
    pub alice: UserId,
    pub oidc: Option<Arc<FakeOidc>>,
}

pub(crate) struct EnvBuilder {
    pub settings: Box<dyn FnOnce(&mut ApiSettings)>,
    pub session: SessionSettings,
    pub oidc: bool,
    pub refresher: Option<Arc<FakeRefresher>>,
    pub rbac: Option<Arc<dyn RbacChecker>>,
    pub api_settings: AuthApiSettings,
    pub carrier: Option<String>,
}

impl EnvBuilder {
    pub(crate) fn new() -> Self {
        Self {
            settings: Box::new(|_| {}),
            session: SessionSettings::default(),
            oidc: false,
            refresher: None,
            rbac: None,
            api_settings: AuthApiSettings::default(),
            carrier: None,
        }
    }

    #[allow(clippy::too_many_lines)] // one harness that builds a whole node in memory
    pub(crate) async fn build(self) -> Env {
        let clock = FixedClock::default();
        let store = Arc::new(MemStore::with_builtin_roles().with_clock(Arc::new(clock.clone())));
        let audit = MemAuditSink::new();
        let auth = FakeAuth::new(clock.clone());
        let users: Arc<dyn UserRepo> = store.clone();
        let alice = users
            .create(&rustryx_testkit::fixtures::user("alice", UserSource::Local))
            .await
            .expect("alice")
            .id;
        let admin = users
            .create(&rustryx_testkit::fixtures::user("admin", UserSource::Local))
            .await
            .expect("admin")
            .id;
        let developer = RoleRef {
            id: rustryx_testkit::builtin_role_id("nx-developer").unwrap_or_else(RoleId::generate),
            name: "nx-developer".to_owned(),
        };
        let admin_role = RoleRef {
            id: rustryx_testkit::builtin_role_id("nx-admin").unwrap_or_else(RoleId::generate),
            name: "nx-admin".to_owned(),
        };
        auth.add(
            "alice",
            Account {
                user_id: alice,
                password: "correct horse".to_owned(),
                source: UserSource::Local,
                roles: vec![developer],
            },
        );
        auth.add(
            "admin",
            Account {
                user_id: admin,
                password: "break glass 123".to_owned(),
                source: UserSource::Local,
                roles: vec![admin_role],
            },
        );
        let sso_user = users
            .create(&rustryx_testkit::fixtures::user(
                "sso-user",
                UserSource::Oidc,
            ))
            .await
            .expect("sso")
            .id;
        auth.add(
            "sso-user",
            Account {
                user_id: sso_user,
                password: "unused".to_owned(),
                source: UserSource::Oidc,
                roles: vec![RoleRef {
                    id: rustryx_testkit::builtin_role_id("nx-developer")
                        .unwrap_or_else(RoleId::generate),
                    name: "nx-developer".to_owned(),
                }],
            },
        );
        let oidc = self.oidc.then(|| {
            let mut port = FakeOidc::new(clock.clone(), sso_user, &["/devs"]);
            if let (Some(carrier), Some(inner)) = (self.carrier.clone(), Arc::get_mut(&mut port)) {
                inner.carrier = Some(carrier);
            }
            port
        });

        let csrf = CsrfKey::from_cookie_key(&Secret::new(base64_key())).expect("csrf key");
        let sessions = Arc::new(SessionManager::new(
            self.session,
            auth.clone(),
            Arc::new(clock.clone()),
            csrf,
            self.refresher
                .clone()
                .map(|r| r as Arc<dyn SsoSessionRefresher>),
        ));
        let mut settings = ApiSettings {
            metrics_access: MetricsAccess::Public,
            require_https: false,
            service_origin: Some(ORIGIN.to_owned()),
            ..ApiSettings::default()
        };
        (self.settings)(&mut settings);
        let lifecycle = Lifecycle::new();
        lifecycle.mark_listening();
        let rbac: Arc<dyn RbacChecker> = self.rbac.unwrap_or_else(|| Arc::new(AdminByName));
        let state = AppState::new(crate::test_support::StubProbe::new(ProbeReport::healthy()))
            .with_lifecycle(lifecycle)
            .with_settings(settings)
            .with_metrics(rustryx_observability::MetricsHandle::new().expect("metrics"))
            .with_authenticator(auth.clone())
            .with_rbac(rbac)
            .with_sessions(sessions);
        let audit_sink: Arc<dyn AuditSink> = Arc::new(audit.clone());
        let mut deps = AuthApiDeps::new(
            store.clone() as Arc<dyn UserRepo>,
            store.clone() as Arc<dyn RoleRepo>,
            store.clone() as Arc<dyn TokenRepo>,
            audit_sink,
            Arc::new(clock.clone()),
        );
        deps.oidc = oidc.clone().map(|o| o as Arc<dyn OidcLoginPort>);
        deps.settings = self.api_settings;
        let (router, catalog) = RouterBuilder::new(state.clone())
            .module(Arc::new(NativeAuthModule::new(deps)))
            .module(crate::test_support::TestModule::new())
            .build_with_catalog();
        Env {
            clock,
            auth,
            store,
            audit,
            state,
            router,
            catalog,
            alice,
            oidc,
        }
    }
}

fn base64_key() -> String {
    use base64::Engine;
    let mut bytes = [3_u8; 32];
    bytes[0] = 9;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// An authorization checker that knows one fact: a principal that holds a role NAMED `nx-admin`
/// is an administrator. (The real one matches ids; the harness only needs a switch.)
pub(crate) struct AdminByName;

#[async_trait]
impl RbacChecker for AdminByName {
    async fn check(
        &self,
        principal: &Principal,
        _action: rustryx_core::identity::Action,
        _resource: &rustryx_core::ports::ResourceRef,
    ) -> Result<rustryx_core::ports::Decision, AuthError> {
        Ok(
            if principal.has_role("nx-admin") || principal.has_role("nx-developer") {
                rustryx_core::ports::Decision::Allow
            } else {
                rustryx_core::ports::Decision::Deny
            },
        )
    }

    async fn can_see_repository(
        &self,
        _principal: &Principal,
        _repository: &str,
        _format: rustryx_core::model::RepoFormat,
    ) -> Result<bool, AuthError> {
        Ok(true)
    }

    async fn is_admin(&self, principal: &Principal) -> Result<bool, AuthError> {
        Ok(principal.has_role("nx-admin"))
    }

    async fn check_app(
        &self,
        principal: &Principal,
        _area: rustryx_core::identity::AppArea,
        _action: rustryx_core::identity::Action,
    ) -> Result<rustryx_core::ports::Decision, AuthError> {
        Ok(if principal.has_role("nx-admin") {
            rustryx_core::ports::Decision::Allow
        } else {
            rustryx_core::ports::Decision::Deny
        })
    }
}

/// A browser: a cookie jar and request helpers that behave like one.
pub(crate) struct Browser<'a> {
    pub env: &'a Env,
    pub cookies: Mutex<HashMap<String, String>>,
}

impl<'a> Browser<'a> {
    pub(crate) fn new(env: &'a Env) -> Self {
        Self {
            env,
            cookies: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    pub(crate) fn set_cookie(&self, name: &str, value: &str) {
        self.cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), value.to_owned());
    }

    fn absorb(&self, headers: &HeaderMap) {
        let mut jar = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
        for value in headers.get_all(http::header::SET_COOKIE) {
            let Ok(text) = value.to_str() else { continue };
            let Some((pair, attributes)) = text.split_once(';') else {
                continue;
            };
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if attributes.contains("Max-Age=0") {
                jar.remove(name);
            } else {
                jar.insert(name.to_owned(), value.to_owned());
            }
        }
    }

    fn cookie_header(&self) -> Option<String> {
        let jar = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
        if jar.is_empty() {
            return None;
        }
        let mut pairs: Vec<String> = jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
        pairs.sort();
        Some(pairs.join("; "))
    }

    /// Sends a request the way a page of the service would: cookies, `Origin`, and the CSRF
    /// header when `csrf` is set.
    pub(crate) async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<serde_json::Value>,
        csrf: bool,
    ) -> Reply {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("origin", ORIGIN);
        if let Some(cookie) = self.cookie_header() {
            builder = builder.header("cookie", cookie);
        }
        if csrf && let Some(token) = self.cookie(crate::CSRF_COOKIE) {
            builder = builder.header("x-csrf-token", token);
        }
        let body = match body {
            Some(json) => {
                builder = builder.header("content-type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let reply = call(&self.env.router, builder.body(body).expect("request")).await;
        self.absorb(&reply.headers);
        reply
    }

    pub(crate) async fn get(&self, uri: &str) -> Reply {
        self.send(Method::GET, uri, None, false).await
    }

    pub(crate) async fn post(&self, uri: &str, body: Option<serde_json::Value>) -> Reply {
        self.send(Method::POST, uri, body, true).await
    }

    pub(crate) async fn login(&self, username: &str, password: &str) -> Reply {
        self.send(
            Method::POST,
            "/api/v1/auth/login",
            Some(serde_json::json!({"username": username, "password": password})),
            false,
        )
        .await
    }

    pub(crate) async fn login_alice(&self) -> Reply {
        let reply = self.login("alice", "correct horse").await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        reply
    }
}

impl Env {
    /// Dependencies for another `NativeAuthModule` over the same stores, for tests that swap
    /// the authenticator or the audit sink.
    pub(crate) fn deps_for_test(&self) -> AuthApiDeps {
        let mut deps = AuthApiDeps::new(
            self.store.clone() as Arc<dyn UserRepo>,
            self.store.clone() as Arc<dyn RoleRepo>,
            self.store.clone() as Arc<dyn TokenRepo>,
            Arc::new(self.audit.clone()),
            Arc::new(self.clock.clone()),
        );
        deps.oidc = self.oidc.clone().map(|o| o as Arc<dyn OidcLoginPort>);
        deps
    }

    /// The time of the harness clock.
    pub(crate) fn clock_now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    /// The clock of the harness as Unix seconds.
    pub(crate) fn clock_unix(&self) -> i64 {
        self.clock.now().unix_timestamp()
    }
}

pub(crate) fn advance(env: &Env, by: Duration) {
    env.clock.advance(by);
}
