# LDAP and Active Directory (`rustryx-auth`, module `ldap`)

LDAP login and LDAP group lookup. The module implements two ports of
`rustryx-core`: `Authenticator` (Basic credentials, bind authentication) and
`GroupResolver` (group DNs of an LDAP principal). It makes no authorization decision:
which role a group DN maps to, and which DN is an administrator group, is the business
of the role mapper in the authentication chain and of `rustryx-rbac`.

Normative sources: README section 8.4 (LDAP) and 8.5 (groups and RBAC), `02-rust-engineering`
sections 5 and 6, `03-security` SEC-08, `01-architecture` section 1.5. README 8 wins over the
other sections where they differ.

Contents: [wiring](#wiring), [configuration](#configuration), [transport and TLS](#transport-and-tls),
[authentication](#authentication), [groups](#groups), [directory notes](#directory-notes),
[observability](#observability), [tests](#tests), [decision records](#decision-records),
[gaps in core and open points](#gaps-in-rustryx-core-and-open-points).

## Wiring

```rust
use rustryx_auth::ldap::{LdapAuthenticator, LdapConfig, LdapGroupResolver, LdapService};

// Field by field copy of the `auth.ldap` section (the names are the same, see below).
let ldap_config: LdapConfig = map_from(&app_config.auth.ldap);

// Startup: a plaintext URL, a bad filter or an unreadable CA file is an Err, and an Err
// must stop the process. Nothing is connected here, so an unreachable directory does not
// stop the node (README 8.2).
ldap_config.validate()?;
let service = LdapService::new(&ldap_config, clock.clone()).await?;

let authenticator = Arc::new(LdapAuthenticator::new(Arc::clone(&service)));
let resolver = Arc::new(LdapGroupResolver::new(Arc::clone(&service)));
// authenticator.is_enabled() / service.is_enabled(): false when `enabled` is false; then
// `authenticate` answers Ok(None) for everything and the chain skips the member.

// Metrics: service.stats().snapshot() (see "Observability").
```

`LdapService::with_transport(config, transport, clock)` builds the same service on any
`LdapTransport` (tests of other crates can use an in-memory directory). The caller then owns
the transport rules: only the transport built by `LdapService::new` enforces TLS.

## Configuration

`LdapConfig` is a flat serde struct with `deny_unknown_fields`: a key that is not in the table,
`allow_plaintext` for example, stops the process instead of being ignored. There is no plaintext
flag. Names marked "same" are identical to the `auth.ldap` section of `rustryx-config`, so the
mapping is a plain copy. The accepted aliases are `start_tls` for `starttls` and `member_search`
for `reverse_member`.

| Field | Default | Meaning and validation | Config crate |
|---|---|---|---|
| `enabled` | `false` | Master switch. A disabled section is not connected, but a plaintext URL in it is still rejected. | same |
| `url` | none | `ldaps://host[:636]`, or `ldap://host[:389]` with `starttls`. No credentials, path, query or fragment. A host is required (it is the name the certificate is checked against). | same (`Option`) |
| `starttls` | `false` | Upgrade `ldap://` with StartTLS. Must be `false` for `ldaps://`. | same |
| `ca_file` | none | PEM file. When set, ONLY these CAs are trusted (see ADR-LDAP-6). When absent the platform trust store is used. | same, different meaning |
| `bind_dn`, `bind_password` | none | Service account. Both required; the password must not be empty or blank. | same |
| `user_base_dn` | none | Subtree searched for users. Required. | same |
| `search_filter` | `(uid={username})` | Must be exactly one filter, contain `{username}` and no other placeholder. | same |
| `username_attribute` | `uid` | Attribute with the canonical user name (`sAMAccountName` for AD). | module only |
| `max_login_length` | `256` | Bytes, 1 to 1024. | module only |
| `group_strategy` | `member_of` | `member_of` or `reverse_member` (`member_search`). | same |
| `group_attribute` | `memberOf` | User (and group) attribute with group DNs. | same |
| `group_base_dn` | none | Required for `reverse_member`. | same |
| `group_filter` | `(member={dn})` | `reverse_member` search for the user. `{dn}` is the user DN, `{username}` the canonical name: `(memberUid={username})` for POSIX groups. | same |
| `nested_group_filter` | `(member={dn})` | `reverse_member`: parents of a group, `{dn}` only. | module only |
| `nested_groups` | `true` | Follow groups that are members of groups. | module only |
| `nested_group_depth` | `5` | 1 to 16. 1 means direct groups only. | same |
| `max_groups` | `1000` | 1 to 10000, per user. | same |
| `groups_cache_ttl_secs` | `60` | 0 to 300. How long a confirmed set is served without asking the directory. | same, other default (300) |
| `groups_stale_on_error` | `false` | Serve the last confirmed set for at most five minutes when the directory is down. See ADR-LDAP-4 before turning it on. | module only |
| `admin_groups` | empty | Accepted and ignored here: the role mapper compares the returned DNs with it by exact equality. | same |
| `connect_timeout_secs`, `bind_timeout_secs`, `search_timeout_secs` | `5` | 1 to 300 each, around EVERY operation. | same |
| `auth_deadline_secs` | `10` | Whole `authenticate` call, group warm-up included. | module only |
| `group_deadline_secs` | `10` | Whole group lookup, nested searches included. | module only |
| `pool_size` | `4` | Service connections open at once, 1 to 64. | module only |
| `pool_max_lifetime_secs` | `300` | A service connection is retired after this, healthy or not. | module only |
| `max_concurrent_user_binds` | `16` | Cap on user bind connections in flight, 1 to 256 (ADR-LDAP-8). | module only |

## Transport and TLS

Only `ldaps://`, or `ldap://` upgraded with StartTLS, is possible, and the certificate chain
AND the host name are verified by the `rustls` default verifier against the configured roots.
Four independent things hold this:

1. `LdapConfig::validate` rejects `ldap://` without `starttls`, `ldaps://` with `starttls`,
   every other scheme and a URL that carries credentials.
2. `LdapService::new` calls it, so a bad section is an error at startup.
3. `NetTransport` builds its endpoint through the same function, so even a caller that skips
   `validate` cannot get a plaintext socket.
4. Nothing in the module calls `ClientConfig::dangerous()` or `LdapConnSettings::set_no_tls_verify`.
   The wire tests prove it: an untrusted CA, a self signed certificate and a certificate for
   another host name each fail the handshake and nothing is sent afterwards, and a refused
   StartTLS never falls back to plaintext.

All network work goes through the `LdapTransport` trait. The callers, not the transport, put
`tokio::time::timeout` around connect, bind, search, health check and close, so a transport that
hangs in any way is cut off; the fake transport in the tests hangs on purpose to prove it.

## Authentication

For `Credentials::Basic` (anything else is `Ok(None)`):

1. Login and password are checked locally, with no network call. Empty, whitespace only and NUL
   passwords are refused (also over 4096 bytes); a login that is empty, too long or has a control
   character is refused. A bind with an empty password is an unauthenticated bind that many
   servers answer with success, so it must not exist: the transport refuses an empty DN or
   password again as a last line.
2. A service connection is taken from the pool (health check on checkout, retired after
   `pool_max_lifetime_secs`, discarded if an operation on it was interrupted) and the user is
   searched with `search_filter`, the login escaped per RFC 4515 and the search capped at two
   entries. Exactly one entry continues; zero, two or a search cut by a size limit is a
   rejection.
3. The connection goes back to the pool, then the password is verified by a simple bind as the
   DN that was found, on a NEW connection that is closed on every path and never pooled.
4. On success the groups are looked up once (so the following `GroupResolver` call is a cache
   hit) and the principal is returned with `external_groups` and `groups_confirmed_at`, or with
   neither when the groups could not be had in the time left. The principal also carries the DN of
   the entry that was bound as in `external_id`. The call never takes longer than
   `auth_deadline_secs`, nor than the request deadline of `AuthContext` when that is shorter: a login
   whose request has run out of time must not keep a directory connection busy.

Everything that is the user's doing (no such user, ambiguous user, wrong password, refused
account, unusable input) is `AuthError::InvalidCredentials` with identical text. A failure of the
directory is `AuthError::ProviderUnavailable` with a fixed message that carries no server text.
"No such user" spends the same network work as "wrong password" (ADR-LDAP-3).

## Groups

Strategies:

- `member_of`: the `group_attribute` values of the user entry, which the user search already
  fetched. Nested groups are read one by one with a base scope search of each group (`memberOf`
  of a group).
- `reverse_member`: a subtree search under `group_base_dn` with `group_filter` for the user, and
  `nested_group_filter` for each group when nesting is on.

The walk is breadth first with three brakes: `nested_group_depth`, a visited set keyed by a
normalised DN (case and spacing around separators folded, so a cycle is cut even when the
directory spells a DN two ways), and `max_groups`. Hitting the depth or count limit is not an
error: the groups found so far are returned (a shorter list can only remove access), the event
is logged at `warn` and counted. A search that fails or times out is `GroupLookupFailed`, never a
shorter or empty list. Particular cases:

- `noSuchObject` while reading a nested group means the group is gone and has no parents: not a
  failure. Any other failure aborts the walk.
- A ranged attribute (`memberOf;range=0-1499`, Active Directory splits long lists that way) is a
  failure: a slice must not pass for the whole list.
- For `reverse_member` the request asks for `max_groups + 1` entries. If the server stops at that
  number the cap is reached (counted); if it stops EARLIER it applied a limit of its own and the
  answer is incomplete, which fails the lookup.

DNs are returned exactly as the directory sent them (first spelling kept, discovery order) in
`ExternalGroups { provider: Ldap, groups, confirmed_at }`. No folding, trimming or sorting happens: the
match with `admin_groups` is exact.

The user DN as a hint (`Principal::external_id`). A refresh that finds the user's DN known (the
principal carries it after an LDAP login, and the resolver remembers the DN of the last lookup next to
the cached groups) reads that entry with scope `base` instead of searching the subtree for the user.
The DN is only a hint and the user filter still decides: the request is the same `search_filter`
rendered with this login, so an account that was disabled, deleted or renamed, or an entry that never
was this user's (a forged hint), answers "nothing" exactly as the subtree search would. Only then is the
subtree searched, because a moved entry is not a gone user; if that finds nothing the user is gone.
Disabled accounts therefore stay excluded at every refresh (see "Directory notes"). A failure other than
`noSuchObject` on the base read is a failed lookup, never a gone user. For `member_of` the base read also
returns the `memberOf` values, so the refresh costs one operation either way, but it is an indexed read
of one entry instead of a subtree search with a filter, it cannot be ambiguous, and `reverse_member`
saves the subtree search for the user as well.

Freshness (README 8.5, the five minute rule):

| Situation | Result |
|---|---|
| A confirmed set younger than `groups_cache_ttl_secs` exists | served without asking the directory |
| Otherwise, the directory answers | new set and new timestamp |
| The directory cannot answer | `Err(GroupLookupFailed)` (default) |
| ... and `groups_stale_on_error` is on, last success at most 300 s ago | the old set, `StaleWithinWindow`, with `confirmed_at` of the last SEARCH |
| ... and it is older | `Err(GroupLookupFailed)`: roles of origin `ldap` are dropped, 403 |
| The directory says the user is gone | the cached set is deleted, `Err` at once, stale mode or not |

`ExternalGroups::confirmed_at` is the time of the directory's answer, also for a set served from the
cache: it is what the chain times its five minute window from. `LdapGroupResolver::resolve_detailed`
returns the same with a `GroupFreshness`. The only state used for these decisions is the resolver's own
cache, measured with the injected `Clock`. The principal's `groups_confirmed_at` is never trusted, and a
timestamp in the future after a clock step is treated as unusable, not as new (the chain applies the same
rule to what the resolver returns, with a 5 second allowance). A principal that is not an LDAP one gets
the empty list confirmed now.

## Directory notes

- Disabled or locked accounts: the user filter is the place to exclude them, and the same filter
  is applied at every group refresh (as a base scope read of the known entry, or as the subtree search
  when the DN is not known). For AD use for example
  `(&(objectClass=user)(sAMAccountName={username})(!(userAccountControl:1.2.840.113556.1.4.803:=2)))`.
- `username_attribute` should be an attribute that `search_filter` matches (`sAMAccountName` in
  the example). The principal gets the directory's spelling when it equals the typed login apart
  from case and padding; any other value would not be found by the refresh, so the typed login is
  kept then (ADR-LDAP-7).
- AD `memberOf` lists direct groups only and omits the primary group (usually `Domain Users`).
  Nesting is followed by this module; the `LDAP_MATCHING_RULE_IN_CHAIN` shortcut is not used.
- AD limits a search to 1000 entries (`MaxPageSize`). Paged search is not implemented: a
  `reverse_member` answer cut by that limit fails the lookup instead of passing as complete.
- Referrals are not followed. A search at a domain root that answers with a referral fails; point
  `user_base_dn` and `group_base_dn` at the naming context of the server asked.
- The service account needs read access to the user entries (`uid`, `memberOf`) and to the group
  entries, nothing else.

## Observability

`LdapService::stats()` returns atomics with `snapshot()`:

| Counter | Meaning |
|---|---|
| `group_depth_limit_hits` | walks that stopped expanding at the depth limit |
| `group_size_limit_hits` | walks that stopped at `max_groups` |
| `group_lookup_failures` | lookups that failed or timed out |
| `groups_served_stale` | stale mode: failed lookups answered from the last confirmed set |
| `groups_expired` | stale mode: failed lookups with nothing younger than five minutes |
| `pool_unhealthy_evictions` | idle connections dropped by the health check |
| `pool_discarded` | connections discarded after an interrupted operation |
| `ambiguous_user_matches` | user searches that matched more than one entry |
| `breaker_trips` | times the circuit breaker opened |
| `breaker_rejections` | service connection attempts refused at once by the open breaker |

The series `ldap_requests_total{result}` goes through the `metrics` facade (`crate::telemetry`, tests in
`hint_tests.rs`): one count per service connection open (connect and service bind), per search and per
user bind. `result` is `ok` when the directory answered (a refused password and `noSuchObject` are
answers), `timeout` when the operation ran into its timeout, `error` for every other failure. A
connection refused at once by the open circuit breaker is not a request and is not counted.
`RustryxLdapErrors` reads `result=~"error|timeout"`.

Logs go through `tracing`. Passwords (the user's and the service account's) are never logged,
never in `Debug` (`Secret` redacts), and server diagnostics stay in the log: the error returned to
a caller is a fixed string. `log_tests.rs` drives every logging path with recognisable secrets and
searches the captured output.

## Tests

```
cargo test -p rustryx-auth --lib ldap::                 # unit, fake transport, in-process LDAP over TLS
RUSTRYX_IT=1 cargo test -p rustryx-auth --lib ldap::integration_tests -- --nocapture
```

- Rules are tested above the `LdapTransport` seam on an in-memory directory whose filter parser is
  strict (only `(attribute=value)` with a cleanly decoded value, else result code 87), so every
  service level test also proves that no input changed the shape of a filter. The fake accepts a
  bind with an empty password on purpose, so a test that asserts "never sent" fails loudly.
- `proptest` covers escaping (RFC 4515 and RFC 4514 round trips, the `ldap3` parser as reference for
  "still one equality filter with this value"), password and login validation, and arbitrary logins
  through the whole login path.
- `hint_tests.rs` covers the user DN hint (base read instead of the subtree search, a forged or stale
  hint, a moved or disabled user), the confirmation time of the resolver, the request deadline and the
  `ldap_requests_total` series.
- `wire_tests.rs` runs the real `ldap3` transport against a small in-process LDAP server over real
  TLS (certificates from `rcgen`): trust, host name, StartTLS, refused StartTLS, bind and search
  result mapping, a malformed entry, a dead connection, and a whole `LdapService::new` login.
- `integration_tests.rs` builds the image of `deploy/compose/ldap`, loads `seed.ldif` (the file
  `docker compose` uses) and runs against real OpenLDAP with a CA and server certificate generated
  by the test: login, escaping with the special user, nested chain, cycle, depth and count limits,
  look alike groups, `reverse_member`, StartTLS, an untrusted CA, a membership change through an
  admin bind, and a directory outage (container stopped). It is skipped, with a message, unless
  `RUSTRYX_IT=1` is set and the fixture files exist. Needs Docker; the image is tagged
  `rustryx-ldap-fixture:it`.

## Decision records

**ADR-LDAP-1. The transport seam, timeouts outside it.** `LdapTransport` and `LdapConnection` are
five operations (connect, bind, search, probe, close). Timeouts are applied by the callers so that
any transport is bounded, and the rules can be tested with a fake that never touches a socket.
`ldap3` is used only through `LdapConnAsync` and its async handle; the connection driver task is
owned by the connection and aborted on drop.

**ADR-LDAP-2. Search, then bind on a throwaway connection.** The user is found with the service
account and verified by binding as the DN found, on a fresh connection that is closed afterwards.
No function accepts a connection from outside the pool and the user bind code never touches the
pool, so a connection that carried a user's password cannot later search as the service account.
Not a flag to forget: the two paths share no state.

**ADR-LDAP-3. Decoy bind.** When there is nobody to bind as (no user, two users) the module binds
to a DN that does not exist, with the password it was given, and ignores the result. Without it
"no such user" answers one connect and one bind sooner than "wrong password", and the difference
is measurable. The decoy can never authenticate: its result is discarded.

**ADR-LDAP-4. Where the five minute window lives.** The authentication chain keeps its own copy of the
last successful answer per user and reuses it for at most five minutes when the resolver fails.
`ExternalGroups::confirmed_at` (Core revision 2, gap 23 closed) makes both timelines run from the
directory's answer: the chain stamps the confirmation time of the answer, not the time it received it, so
a set served from the resolver's 60 second cache is already as old as it is, and the two graces cannot add
up. The total staleness of a group set is at most five minutes from the directory's answer, whichever
layer serves it (the resolver cache, the chain's copy, the principal cache). The resolver stays strict by
default (`groups_stale_on_error` off, `groups_cache_ttl_secs` 60): a stale `Ok` from here would hide the
outage from the chain's failure handling and log, and the chain's copy already gives the grace. Tests:
`chain/tests_window.rs` (the window never exceeds five minutes in total, an older answer or one from the
future is refused). `groups_stale_on_error` remains for callers that have no grace of their own.

**ADR-LDAP-5. Limits are partial, failures are errors.** Depth and count limits return what was
found and are logged and counted. A failed, timed out, ranged or server truncated search fails the
lookup. Only a shorter list can follow from a limit, which can only remove access.

**ADR-LDAP-6. Trust is the CA file, alone.** With `ca_file` only those CAs are trusted, so a
directory pinned to an internal CA is not also trusted through every public CA. `rustryx-config`
documents the file as "in addition to system roots"; the two must be reconciled, this module follows
the stricter reading. The TLS provider is named explicitly (`aws-lc-rs`): the default provider panics
when both `ring` and `aws-lc-rs` are compiled in, which this workspace does (`openidconnect` pulls in
`ring`). Platform roots are loaded once at startup in `spawn_blocking`; `ldap3` would otherwise read them
lazily inside the first login.

**ADR-LDAP-7. Canonical user name and the refresh invariant.** The principal is named after the
directory's spelling so that one person is one account, but only when it is the same name apart from
case and padding. The group refresh searches again with `search_filter` and that name, so a name the
filter cannot find would fail every refresh; keeping the typed login then is the safe choice. Log in
with one kind of identifier if the filter matches several attributes.

**ADR-LDAP-8. Bounded user binds.** Each login opens its own connection. A semaphore caps them
(`max_concurrent_user_binds`), so a storm of logins queues inside the authentication deadline instead of
opening that many TLS connections to the directory. Exhaustion shows as `ProviderUnavailable` (deadline).

**ADR-LDAP-9. One flat, strict configuration shape.** Same field names as `rustryx-config`, so the
composition root copies fields; `deny_unknown_fields` makes a plaintext or "skip verification" key an
error at startup. Validation parses everything once into `LdapSettings` (URL, filter templates,
durations), so nothing downstream sees an invalid value.

**ADR-LDAP-10. Circuit breaker on new service connections (SEC-08).** Five failures in a row (connect,
TLS, timeout or service bind) open it for ten seconds; then one trial attempt is let through, and success
closes it. While it is open a login fails at once (`ProviderUnavailable`) instead of spending a connect
timeout, and a wrong service password is not retried on every login until the directory locks the account.
Only the creation of a connection passes through it: warm, healthy pooled connections keep working. The
thresholds are constants (`breaker.rs`), not configuration, because a wrong value is worse than the default
for every deployment this module has to serve. A cancelled trial counts as a failure.

**ADR-LDAP-11. The DN is a hint, the filter decides.** Core revision 2 gave `Principal` an `external_id`
(gap 25 closed), and the LDAP authenticator puts the DN of the bound entry there. The refresh reads that
entry directly (scope `base`) with the same user filter, remembers the DN of the last lookup next to the
cached groups so that a session that no longer carries the principal still benefits, and falls back to the
subtree search when the hint does not lead to the user. Trusting the DN alone would save one more
operation but would end the exclusion of disabled accounts at refresh, which docs/ldap.md section
"Directory notes" relies on; a forged DN could not read another user's groups either way, because the
filter pins the login.

## Gaps in `rustryx-core` and open points

Closed by Core revision 2: `ExternalGroups::confirmed_at` (ADR-LDAP-4) and the user DN on the principal
(ADR-LDAP-11).

1. `AuthError::Overloaded` ("503, no permit before the deadline") is not produced: exhaustion of the
   service pool or the user bind cap ends in the authentication deadline, `ProviderUnavailable`. If the
   HTTP layer should answer 503 with `Retry-After` for it, the mapping belongs there.
2. `rustryx-config` differs in three places: `ca_file` semantics (ADR-LDAP-6), the default of
   `groups_cache_ttl_secs` (300 there, 60 here; with 300 and strict mode an outage fails closed right
   after the TTL, and the chain's own grace is the only one), and the strategy name `member_search`
   (accepted as an alias).
3. No fuzz target exists for the filter builder (`02-rust-engineering` section 6). The inputs are
   `rustryx_auth::ldap::escape_filter_value` and `filter::FilterTemplate::render`; a target in the
   workspace `fuzz` directory is a few lines.
4. Not done: paged search for answers above a server page size, referral following, several URLs for
   failover. The circuit breaker is per process: with N nodes the directory sees up to N trial attempts per
   cooldown.
