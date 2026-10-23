//! Test doubles: an in-memory directory behind the `LdapTransport` seam, a settable
//! clock, and a harness that wires a service, authenticator and resolver on them.
//!
//! The fake directory is strict about filters on purpose. It accepts only
//! `(attribute=value)` with a value that decodes cleanly (no raw `(`, `)`, `*` or NUL),
//! plus `(attribute=*)`. A filter any looser than that comes back as result code 87
//! (bad filter), so every service level test also proves that no input ever changed
//! the shape of a filter.
//!
//! It is also deliberately dangerous in one respect: a simple bind with an EMPTY
//! password succeeds, as it does on many real servers. A test that asserts the bind
//! never happened therefore fails loudly if a code path ever lets one through.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, Principal, UserSource};
use rustryx_core::ports::Clock;
use time::OffsetDateTime;
use time::macros::datetime;

use super::authenticator::LdapAuthenticator;
use super::config::LdapConfig;
use super::dn::dn_key;
use super::filter::unescape_filter_value;
use super::resolver::LdapGroupResolver;
use super::service::LdapService;
use super::transport::{
    LdapConnection, LdapEntry, LdapTransport, SearchRequest, SearchResponse, SearchScope,
    TransportError,
};

pub(crate) const PEOPLE_DN: &str = "ou=people,dc=example,dc=org";
pub(crate) const GROUPS_DN: &str = "ou=groups,dc=example,dc=org";
pub(crate) const SERVICE_DN: &str = "cn=svc,dc=example,dc=org";
pub(crate) const SERVICE_PASSWORD: &str = "service-secret";
pub(crate) const T0: OffsetDateTime = datetime!(2026-10-24 12:00:00 UTC);

pub(crate) fn user_dn(uid: &str) -> String {
    format!("uid={uid},{PEOPLE_DN}")
}

pub(crate) fn group_dn(cn: &str) -> String {
    format!("cn={cn},{GROUPS_DN}")
}

/// Asserts that `items` is empty and shows them when it is not.
// `assert!(items.is_empty())` is what `assert_is_empty` dislikes; this helper exists so the
// failure message names what was found, which the lint's suggested `assert_eq!` against an
// empty array literal prints far less readably.
#[allow(clippy::assert_is_empty)]
pub(crate) fn assert_empty<T: std::fmt::Debug>(items: &[T], what: &str) {
    assert!(items.is_empty(), "{what}: {items:?}");
}

/// Asserts that `items` is not empty.
#[allow(clippy::assert_is_empty)] // same helper rationale as `assert_empty`
pub(crate) fn assert_not_empty<T: std::fmt::Debug>(items: &[T], what: &str) {
    assert!(!items.is_empty(), "{what}: nothing found");
}

/// A clock a test moves by hand.
pub(crate) struct FakeClock(Mutex<OffsetDateTime>);

impl FakeClock {
    pub(crate) fn new(at: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self(Mutex::new(at)))
    }

    pub(crate) fn advance(&self, by: time::Duration) {
        *self.0.lock().unwrap() += by;
    }

    pub(crate) fn set(&self, at: OffsetDateTime) {
        *self.0.lock().unwrap() = at;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}

/// The directory content.
#[derive(Clone)]
pub(crate) struct Directory {
    entries: Vec<LdapEntry>,
    credentials: Vec<(String, String)>,
    memberof_overlay: bool,
}

impl Directory {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            credentials: vec![(dn_key(SERVICE_DN), SERVICE_PASSWORD.to_owned())],
            memberof_overlay: true,
        }
    }

    pub(crate) fn user(mut self, uid: &str, password: &str) -> Self {
        let dn = user_dn(uid);
        self.credentials.push((dn_key(&dn), password.to_owned()));
        self.entries.push(
            LdapEntry::new(dn)
                .with_attribute("uid", [uid])
                .with_attribute("objectClass", ["person"]),
        );
        self
    }

    /// A `groupOfNames`; `members` are full DNs (users or other groups).
    pub(crate) fn group(mut self, cn: &str, members: &[&str]) -> Self {
        self.entries.push(
            LdapEntry::new(group_dn(cn))
                .with_attribute("objectClass", ["groupOfNames"])
                .with_attribute("member", members.iter().copied()),
        );
        self
    }

    /// A POSIX group listing user names.
    pub(crate) fn posix_group(mut self, cn: &str, member_uids: &[&str]) -> Self {
        self.entries.push(
            LdapEntry::new(group_dn(cn))
                .with_attribute("objectClass", ["posixGroup"])
                .with_attribute("memberUid", member_uids.iter().copied()),
        );
        self
    }

    pub(crate) fn entry(mut self, entry: LdapEntry) -> Self {
        self.entries.push(entry);
        self
    }

    pub(crate) fn without_memberof_overlay(mut self) -> Self {
        self.memberof_overlay = false;
        self
    }

    pub(crate) fn remove_member(&mut self, group: &str, member: &str) {
        let group = dn_key(group);
        for entry in &mut self.entries {
            if dn_key(&entry.dn) != group {
                continue;
            }
            for (name, values) in &mut entry.attributes {
                if name.eq_ignore_ascii_case("member") {
                    values.retain(|value| dn_key(value) != dn_key(member));
                }
            }
        }
    }

    pub(crate) fn remove_entry(&mut self, dn: &str) {
        self.entries.retain(|entry| dn_key(&entry.dn) != dn_key(dn));
    }

    fn member_of(&self, dn: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|group| match group.values("member") {
                super::transport::AttributeValues::Complete(values) => {
                    values.iter().any(|value| dn_key(value) == dn_key(dn))
                }
                _ => false,
            })
            .map(|group| group.dn.clone())
            .collect()
    }
}

/// Faults injected into operations.
// A bag of independent fault switches: an enum would only be able to model one fault at a
// time, and several tests combine them (a slow search followed by a hanging user bind).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub(crate) struct Knobs {
    pub(crate) hang_connect: bool,
    pub(crate) fail_connect: bool,
    pub(crate) hang_bind: bool,
    /// Hangs only binds that are not the service account's.
    pub(crate) hang_user_bind: bool,
    /// Delays only binds that are not the service account's.
    pub(crate) user_bind_delay: Option<Duration>,
    pub(crate) hang_search: bool,
    pub(crate) hang_health_check: bool,
    pub(crate) bind_error: Option<TransportError>,
    /// Fails only binds that are not the service account's.
    pub(crate) user_bind_error: Option<TransportError>,
    pub(crate) search_error: Option<TransportError>,
    pub(crate) search_error_for_base: Vec<(String, u32)>,
    pub(crate) server_size_limit: Option<usize>,
    pub(crate) search_delay: Option<Duration>,
    /// Connections with an id up to this one report unhealthy.
    pub(crate) dead_up_to: usize,
    pub(crate) search_size_limit_flag_always: bool,
}

/// What happened on the fake wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Connect {
        conn: usize,
    },
    Bind {
        conn: usize,
        dn: String,
        password: String,
    },
    Search {
        conn: usize,
        request: SearchRequest,
    },
    HealthCheck {
        conn: usize,
    },
    Close {
        conn: usize,
    },
    Dropped {
        conn: usize,
    },
}

struct Shared {
    directory: Mutex<Directory>,
    knobs: Mutex<Knobs>,
    events: Mutex<Vec<Event>>,
    next_id: AtomicUsize,
    user_binds_now: AtomicUsize,
    user_binds_max: AtomicUsize,
}

/// The fake transport. Clone it freely: all clones share one state.
#[derive(Clone)]
pub(crate) struct FakeTransport {
    shared: Arc<Shared>,
}

impl FakeTransport {
    pub(crate) fn new(directory: Directory) -> Self {
        Self {
            shared: Arc::new(Shared {
                directory: Mutex::new(directory),
                knobs: Mutex::new(Knobs::default()),
                events: Mutex::new(Vec::new()),
                next_id: AtomicUsize::new(1),
                user_binds_now: AtomicUsize::new(0),
                user_binds_max: AtomicUsize::new(0),
            }),
        }
    }

    pub(crate) fn knobs(&self) -> MutexGuard<'_, Knobs> {
        self.shared.knobs.lock().unwrap()
    }

    pub(crate) fn update_directory(&self, change: impl FnOnce(&mut Directory)) {
        change(&mut self.shared.directory.lock().unwrap());
    }

    pub(crate) fn kill_existing_connections(&self) {
        let newest = self.shared.next_id.load(Ordering::SeqCst) - 1;
        self.knobs().dead_up_to = newest;
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        self.shared.events.lock().unwrap().clone()
    }

    pub(crate) fn clear_events(&self) {
        self.shared.events.lock().unwrap().clear();
    }

    pub(crate) fn binds(&self) -> Vec<(usize, String, String)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Bind { conn, dn, password } => Some((conn, dn, password)),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn searches(&self) -> Vec<(usize, SearchRequest)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Search { conn, request } => Some((conn, request)),
                _ => None,
            })
            .collect()
    }

    /// The highest number of user bind connections that were mid bind at the same time.
    pub(crate) fn max_concurrent_user_binds(&self) -> usize {
        self.shared.user_binds_max.load(Ordering::SeqCst)
    }

    pub(crate) fn was_closed(&self, conn: usize) -> bool {
        self.events().contains(&Event::Close { conn })
    }

    /// Connection ids that carried a bind as `dn`.
    pub(crate) fn connections_bound_as(&self, dn: &str) -> Vec<usize> {
        self.binds()
            .into_iter()
            .filter(|(_, bound, _)| dn_key(bound) == dn_key(dn))
            .map(|(conn, _, _)| conn)
            .collect()
    }

    fn record(&self, event: Event) {
        self.shared.events.lock().unwrap().push(event);
    }
}

#[async_trait]
impl LdapTransport for FakeTransport {
    async fn connect(&self) -> Result<Box<dyn LdapConnection>, TransportError> {
        let knobs = self.knobs().clone();
        if knobs.hang_connect {
            std::future::pending::<()>().await;
        }
        if knobs.fail_connect {
            return Err(TransportError::Connect("connection refused".to_owned()));
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        self.record(Event::Connect { conn: id });
        Ok(Box::new(FakeConnection {
            id,
            transport: self.clone(),
        }))
    }
}

/// Counts user binds in flight and remembers the peak.
struct UserBindGauge<'a>(&'a Shared);

impl<'a> UserBindGauge<'a> {
    fn enter(shared: &'a Shared) -> Self {
        let now = shared.user_binds_now.fetch_add(1, Ordering::SeqCst) + 1;
        shared.user_binds_max.fetch_max(now, Ordering::SeqCst);
        Self(shared)
    }
}

impl Drop for UserBindGauge<'_> {
    fn drop(&mut self) {
        self.0.user_binds_now.fetch_sub(1, Ordering::SeqCst);
    }
}

struct FakeConnection {
    id: usize,
    transport: FakeTransport,
}

impl Drop for FakeConnection {
    fn drop(&mut self) {
        self.transport.record(Event::Dropped { conn: self.id });
    }
}

#[async_trait]
impl LdapConnection for FakeConnection {
    async fn simple_bind(&mut self, dn: &str, password: &str) -> Result<(), TransportError> {
        self.transport.record(Event::Bind {
            conn: self.id,
            dn: dn.to_owned(),
            password: password.to_owned(),
        });
        let knobs = self.transport.knobs().clone();
        if knobs.hang_bind {
            std::future::pending::<()>().await;
        }
        if let Some(error) = knobs.bind_error {
            return Err(error);
        }
        let is_service = dn_key(dn) == dn_key(SERVICE_DN);
        let _in_flight = (!is_service).then(|| UserBindGauge::enter(&self.transport.shared));
        if !is_service && knobs.hang_user_bind {
            std::future::pending::<()>().await;
        }
        if let (false, Some(delay)) = (is_service, knobs.user_bind_delay) {
            tokio::time::sleep(delay).await;
        }
        if !is_service && let Some(error) = knobs.user_bind_error {
            return Err(error);
        }
        if password.is_empty() {
            // The trap: an unauthenticated bind that "succeeds".
            return Ok(());
        }
        let accepted = self
            .transport
            .shared
            .directory
            .lock()
            .unwrap()
            .credentials
            .iter()
            .any(|(known_dn, known_password)| {
                *known_dn == dn_key(dn) && known_password == password
            });
        if accepted {
            Ok(())
        } else {
            Err(TransportError::Server {
                code: 49,
                message: "invalid credentials".to_owned(),
            })
        }
    }

    async fn search(&mut self, request: &SearchRequest) -> Result<SearchResponse, TransportError> {
        self.transport.record(Event::Search {
            conn: self.id,
            request: request.clone(),
        });
        let knobs = self.transport.knobs().clone();
        if knobs.hang_search {
            std::future::pending::<()>().await;
        }
        if let Some(delay) = knobs.search_delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(error) = knobs.search_error {
            return Err(error);
        }
        if let Some((_, code)) = knobs
            .search_error_for_base
            .iter()
            .find(|(base, _)| dn_key(base) == dn_key(&request.base))
        {
            return Err(TransportError::Server {
                code: *code,
                message: "injected".to_owned(),
            });
        }
        let directory = self.transport.shared.directory.lock().unwrap().clone();
        run_search(&directory, request, &knobs)
    }

    async fn is_healthy(&mut self) -> bool {
        self.transport.record(Event::HealthCheck { conn: self.id });
        let knobs = self.transport.knobs().clone();
        if knobs.hang_health_check {
            std::future::pending::<()>().await;
        }
        self.id > knobs.dead_up_to
    }

    async fn close(self: Box<Self>) {
        self.transport.record(Event::Close { conn: self.id });
    }
}

#[derive(Debug)]
enum Match {
    Equals { attribute: String, value: String },
    Present { attribute: String },
}

/// Parses the only two filter shapes the fake accepts.
fn parse_filter(filter: &str) -> Option<Match> {
    let inner = filter.strip_prefix('(')?.strip_suffix(')')?;
    let (attribute, value) = inner.split_once('=')?;
    if attribute.is_empty() || !attribute.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    if value == "*" {
        return Some(Match::Present {
            attribute: attribute.to_owned(),
        });
    }
    Some(Match::Equals {
        attribute: attribute.to_owned(),
        value: unescape_filter_value(value)?,
    })
}

fn run_search(
    directory: &Directory,
    request: &SearchRequest,
    knobs: &Knobs,
) -> Result<SearchResponse, TransportError> {
    let bad_filter = || TransportError::Server {
        code: 87,
        message: format!("bad filter: {}", request.filter),
    };
    let matcher = parse_filter(&request.filter).ok_or_else(bad_filter)?;
    let base = dn_key(&request.base);
    if request.scope == SearchScope::Base {
        let exists = base.is_empty()
            || directory
                .entries
                .iter()
                .any(|entry| dn_key(&entry.dn) == base);
        if !exists {
            return Err(TransportError::Server {
                code: 32,
                message: "no such object".to_owned(),
            });
        }
    }
    let mut found: Vec<LdapEntry> = directory
        .entries
        .iter()
        .filter(|entry| {
            in_scope(entry, &base, request.scope) && matches(directory, entry, &matcher)
        })
        .map(|entry| project(directory, entry, &request.attributes))
        .collect();
    let limit = [
        request.size_limit.map(|l| l as usize),
        knobs.server_size_limit,
    ]
    .into_iter()
    .flatten()
    .min();
    let mut size_limit_exceeded = knobs.search_size_limit_flag_always;
    if let Some(limit) = limit
        && found.len() > limit
    {
        found.truncate(limit);
        size_limit_exceeded = true;
    }
    Ok(SearchResponse {
        entries: found,
        size_limit_exceeded,
    })
}

fn in_scope(entry: &LdapEntry, base: &str, scope: SearchScope) -> bool {
    let key = dn_key(&entry.dn);
    match scope {
        SearchScope::Base => key == base,
        SearchScope::Subtree => {
            base.is_empty() || key == base || key.ends_with(&format!(",{base}"))
        }
        SearchScope::OneLevel => key
            .strip_suffix(&format!(",{base}"))
            .is_some_and(|rdn| !rdn.contains(',')),
    }
}

fn matches(directory: &Directory, entry: &LdapEntry, matcher: &Match) -> bool {
    match matcher {
        Match::Present { attribute } => {
            attribute.eq_ignore_ascii_case("objectClass")
                || !values_of(directory, entry, attribute).is_empty()
        }
        Match::Equals { attribute, value } => {
            values_of(directory, entry, attribute)
                .iter()
                .any(|candidate| {
                    if attribute.eq_ignore_ascii_case("member")
                        || attribute.eq_ignore_ascii_case("memberOf")
                    {
                        dn_key(candidate) == dn_key(value)
                    } else {
                        candidate.eq_ignore_ascii_case(value)
                    }
                })
        }
    }
}

fn values_of(directory: &Directory, entry: &LdapEntry, attribute: &str) -> Vec<String> {
    if attribute.eq_ignore_ascii_case("memberOf") && directory.memberof_overlay {
        return directory.member_of(&entry.dn);
    }
    match entry.values(attribute) {
        super::transport::AttributeValues::Complete(values) => {
            values.into_iter().map(str::to_owned).collect()
        }
        _ => Vec::new(),
    }
}

fn project(directory: &Directory, entry: &LdapEntry, attributes: &[String]) -> LdapEntry {
    let mut projected = LdapEntry::new(entry.dn.clone());
    let wants_all = attributes.iter().any(|a| a == "*");
    let names: Vec<&str> = if wants_all {
        entry
            .attributes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    } else {
        attributes
            .iter()
            .map(String::as_str)
            .filter(|a| *a != "1.1")
            .collect()
    };
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        let values = values_of(directory, entry, name);
        if !values.is_empty() {
            projected = projected.with_attribute(name, values);
        }
    }
    projected
}

/// The standard directory most tests start from.
pub(crate) fn standard_directory() -> Directory {
    Directory::new()
        .user("alice", "alice-pw")
        .user("bob", "bob-pw")
        .group("devs", &[&user_dn("alice")])
        .group("admins", &[&user_dn("alice"), &user_dn("bob")])
}

pub(crate) fn base_config() -> LdapConfig {
    LdapConfig {
        enabled: true,
        url: Some("ldaps://ldap.example.org".into()),
        bind_dn: Some(SERVICE_DN.into()),
        bind_password: Some(Secret::new(SERVICE_PASSWORD)),
        user_base_dn: Some(PEOPLE_DN.into()),
        ..LdapConfig::default()
    }
}

/// A service, authenticator and resolver on the fake transport and fake clock.
pub(crate) struct Harness {
    pub(crate) fake: FakeTransport,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) service: Arc<LdapService>,
    pub(crate) auth: LdapAuthenticator,
    pub(crate) resolver: LdapGroupResolver,
}

impl Harness {
    pub(crate) fn new(config: &LdapConfig, directory: Directory) -> Self {
        let fake = FakeTransport::new(directory);
        let clock = FakeClock::new(T0);
        let service = LdapService::with_transport(
            config,
            Arc::new(fake.clone()),
            Arc::clone(&clock) as Arc<dyn Clock>,
        )
        .unwrap();
        Self {
            auth: LdapAuthenticator::new(Arc::clone(&service)),
            resolver: LdapGroupResolver::new(Arc::clone(&service)),
            fake,
            clock,
            service,
        }
    }

    pub(crate) fn standard() -> Self {
        Self::new(&base_config(), standard_directory())
    }

    pub(crate) async fn login(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Principal>, rustryx_core::AuthError> {
        use rustryx_core::ports::Authenticator;
        self.auth
            .authenticate(&basic(user, password), &AuthContext::default())
            .await
    }
}

pub(crate) fn basic(user: &str, password: &str) -> Credentials {
    Credentials::Basic {
        username: user.to_owned(),
        password: Secret::new(password),
    }
}

/// A principal as the chain would hand it to the resolver.
pub(crate) fn ldap_principal(username: &str) -> Principal {
    Principal::new(username, UserSource::Ldap, AuthMethod::BasicLdap)
}

/// A transport whose searches return canned answers in order, for response shapes the
/// fake directory cannot produce (ranged attributes, empty DNs, odd flags).
#[derive(Clone)]
pub(crate) struct ScriptedTransport {
    answers: Arc<Mutex<std::collections::VecDeque<Result<SearchResponse, TransportError>>>>,
}

impl ScriptedTransport {
    pub(crate) fn new(answers: Vec<Result<SearchResponse, TransportError>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into())),
        }
    }
}

struct ScriptedConnection {
    answers: Arc<Mutex<std::collections::VecDeque<Result<SearchResponse, TransportError>>>>,
}

#[async_trait]
impl LdapTransport for ScriptedTransport {
    async fn connect(&self) -> Result<Box<dyn LdapConnection>, TransportError> {
        Ok(Box::new(ScriptedConnection {
            answers: Arc::clone(&self.answers),
        }))
    }
}

#[async_trait]
impl LdapConnection for ScriptedConnection {
    async fn simple_bind(&mut self, _dn: &str, password: &str) -> Result<(), TransportError> {
        if password == SERVICE_PASSWORD || password == "user-pw" {
            Ok(())
        } else {
            Err(TransportError::Server {
                code: 49,
                message: "invalid credentials".to_owned(),
            })
        }
    }

    async fn search(&mut self, _request: &SearchRequest) -> Result<SearchResponse, TransportError> {
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(TransportError::Closed))
    }

    async fn is_healthy(&mut self) -> bool {
        true
    }

    async fn close(self: Box<Self>) {}
}

impl Harness {
    /// A harness over a scripted transport.
    pub(crate) fn scripted(config: &LdapConfig, transport: ScriptedTransport) -> Self {
        let clock = FakeClock::new(T0);
        let service = LdapService::with_transport(
            config,
            Arc::new(transport),
            Arc::clone(&clock) as Arc<dyn Clock>,
        )
        .unwrap();
        Self {
            auth: LdapAuthenticator::new(Arc::clone(&service)),
            resolver: LdapGroupResolver::new(Arc::clone(&service)),
            fake: FakeTransport::new(Directory::new()),
            clock,
            service,
        }
    }
}
