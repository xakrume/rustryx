# Authentication core (`rustryx-auth`)

This document covers the provider independent half of `rustryx-auth`: local
passwords, API tokens, the internal session JWT, the login guard, the group to role
mapping, just-in-time provisioning and the chain that ties them together. The
Keycloak and LDAP modules implement `Authenticator` and `GroupResolver` and are
described elsewhere.

The crate answers "who is calling and which external groups do they have". It never
decides what they may do: that is `rustryx-rbac`.

Status: adapted to Core revision 2 (docs/core-revision-2-migration.md, section 3.4). The
gaps 16 to 25 and 32 of docs/core-gaps.md are closed here; the table at the end of the
decisions lists where.

## The chain

`AuthService` implements `Authenticator` itself, so the HTTP layer sees one
authenticator. Members run in a fixed order and each returns `Ok(None)` for
credentials that are not its kind:

1. local Basic (`LocalPasswordAuthenticator`): `Basic` for users with source `local`;
2. API token (`TokenAuthenticator`): `Bearer ryx_*` and `Basic <any>:ryx_*`;
3. internal JWT (`InternalJwtAuthenticator`): `Bearer` JWT, alg `EdDSA`, `iss` = internal issuer;
4. OIDC bearer (injected): Keycloak access tokens;
5. LDAP (injected): `Basic` credentials bound against the directory.

The first member that claims the credentials decides. An `Err` from a member that
claimed them is final, so a wrong local password is never retried against LDAP.
The service never returns `Ok(None)`: anonymous credentials give
`Principal::anonymous()`, unclaimed Basic gives `InvalidCredentials`, unclaimed
Bearer gives `InvalidToken`.

Around the members the service adds:

- the login guard around password Basic credentials;
- the dummy argon2id when nobody claims Basic credentials;
- the 60 second cache of verified Basic passwords of local accounts;
- just-in-time provisioning of external accounts;
- group resolution, the shared group to role mapping and persistence of external roles;
- the principal cache for JWT bearers;
- the request deadline of `AuthContext`, passed to every blocking hash;
- the metrics of an authentication (see "Metrics").

`authenticate_detailed` is overridden: a refusal is an `AuthFailure { method, error }`
where `method` names the member that refused (`None` when the credentials were refused
before any member looked at them: a locked account, blank credentials, nobody claimed
them). The plain `authenticate` is the same decision without the method.

### Where roles come from

Roles are computed from what the current request proves. Persisted `user_roles`
rows are written for the UI and for audit and are never read back as a grant.

| caller | account | groups | roles |
|---|---|---|---|
| local (Basic, token, JWT) | stored | none | rows of origin `manual` |
| OIDC bearer | JIT by `(iss, sub)` | `groups` claim through the OIDC resolver | mapped, this request only |
| OIDC UI session (internal JWT) | stored | snapshot inside the JWT, at most 5 minutes old | mapped |
| API token of an OIDC user | stored | none can be confirmed | `manual` rows only when supplement is on |
| LDAP (Basic, token, JWT) | JIT by username | LDAP resolver, per request | mapped |
| robot or scan token | none | none | none: the rights are the token scopes alone |

A missing or empty OIDC `groups` claim removes OIDC-origin roles for that request.
An LDAP lookup that fails reuses the previous successful result only while it is at
most 5 minutes old, counted from the directory's answer; after that the request is
refused. A failed lookup is never an empty group list.

An identity that is real but maps to no role and has no default role, or that may not be
provisioned (`manual` or `allowlist` mode), is refused with `AuthError::AccessDenied` (403).

Robot and scan tokens (`AuthMethod::{RobotToken, ScanToken}`) have no authenticator yet: they
come with the streams that own their tables, and share the `ryx_` shape with personal tokens, so
the kind is whichever table knows the hash. What this crate does today is classify and refuse
safely: a `ryx_` token that no table knows is `unknown API token`, whatever it would have been; a
principal that a future authenticator hands to the chain with one of these methods gets no roles
and no account, and one without `token_scopes` is refused (a token principal without scopes reads
as an unrestricted personal token to the authorizer); a principal with a method the chain does not
know, or with `Anonymous`, is refused with a backend error instead of becoming an account with no
roles.

## Passwords (`passwords`)

- New hashes: argon2id v19, 16 byte random salt, PHC strings. Defaults `m = 64 MiB,
  t = 3, p = 1`. `PasswordConfig::validated` refuses anything below the OWASP floor
  (19 MiB, t = 2, p = 1). Peak memory is `password permits x m`: 8 cores x 64 MiB =
  512 MiB, size the pod for it or lower `m` toward the floor.
- Imported `$2a$`, `$2b$` and `$2y$` bcrypt hashes verify; `$2x$` and argon2i/argon2d do not.
  Every bcrypt hash and every argon2id hash below the configured parameters reports
  `needs_rehash`; `LocalPasswordAuthenticator` then rewrites it with
  `UserRepo::set_password_hash_if(id, verified_hash, new_hash)`, a compare and set: a password
  changed after the login read its row is never overwritten with a hash of the old one. Parameters
  are only raised, never lowered.
- Blank (empty or whitespace only) and oversized (> 1024 bytes) passwords are refused
  before any work: no hash, no gate permit, no database read.
- `Passwords::verify_dummy` runs a real verification with the configured parameters
  under the same semaphore, for credentials that name no account.
- `hash_before`, `verify_before` and `verify_dummy_before` take the request deadline
  (`AuthContext::deadline`); `hash`, `verify` and `verify_dummy` are the same without one.
- `validate_new_password`: 12 to 1024 bytes, not blank, must not start with `ryx_`
  (see ADR-7). The offline list of compromised passwords is applied by the API layer.

### `BlockingGate`

Two lanes, each a `tokio::sync::Semaphore`: passwords (physical cores) and general
(2 x logical CPUs). Invariants:

1. the permit is taken before `spawn_blocking`;
2. the permit moves into the job and is released when the job ends, not when the
   caller stops waiting (a cancelled caller leaves the CPU busy);
3. waiters are bounded too: past `max_waiting` the call fails at once, and no waiter
   lives longer than `permit_wait`;
4. no wait outlives the request deadline: `acquire_before` and `run_before` wait for the shorter
   of `permit_wait` and the time left, and a deadline that has already passed refuses the call
   before it takes a permit, even a free one (the request is dead, and the core is better used by a
   live one).

No permit in time gives `AuthError::Overloaded`, which the HTTP layer maps to 503 with
`Retry-After`.

## The cache of verified Basic passwords (SEC-07)

Every Basic request of a local account pays one argon2id verification. The chain remembers
"this exact user and password verified a moment ago" for at most 60 seconds
(`AuthServiceConfig::basic_cache_ttl`, clamped to 60 s, zero turns it off). Code:
`chain/basic_cache.rs`, tests `chain/tests_basic_cache.rs`.

- Never plaintext. The key is `HMAC-SHA256(process key, label ++ len(user) ++ user ++ password)`
  with a random 32 byte key per process start; the user is folded like the login guard folds it.
  The entry holds a principal, the user id and a SHA-256 fingerprint of the stored password hash.
- Never from memory alone. A hit re-reads the account and serves only when the row still says what
  it said at verification: the account exists, is local and enabled, its stored hash is the verified
  one (a password change moves it) and `tokens_valid_after` has not moved. Anything else drops the
  entry and the request takes the full path, which then gives the real answer. A hit that cannot read
  the row is a miss, and the cache bookkeeping never fails a request: a store outage is reported by the
  verification that follows, with the right method. A password change,
  a disabled account or a revocation therefore never depends on an invalidation event arriving; the
  events (`User`, `Rbac`, `Reset`) only free memory sooner.
- Never a role. Roles are computed by `finish` on every request, hit or not.
- Never an LDAP bind. The directory owns that password; a change or a disabled account there cannot
  be observed from here, and a bind costs round trips, not a hash.
- Never around the guard. `LoginGuard::check` runs before the lookup, so a locked account stays
  locked for the right password too, and a wrong password (another key) takes the verification path
  and is counted. The optional guard cache hook is not built: the check is one indexed read, and a
  cached answer would let a lockout lag behind the counters.
- Written only from a verification that provably used the row it describes: the row is read before
  and after the verification and nothing is stored unless both reads carry the same hash and
  `tokens_valid_after`. A password changed during the verification cannot leave the old password
  cached under the new hash, and a login that rewrote the hash (bcrypt import) is remembered by the
  next login, not by itself.

## API tokens (`tokens`)

`ryx_` + 32 CSPRNG bytes as base64url (47 characters). Only the SHA-256 hex is
stored; the secret is returned once by `create_token`. Lifetime is mandatory:
90 days by default, 365 days at most. The authenticator checks format, looks the
hash up through `TokenRepo`, compares the stored and computed hash in constant time,
then checks revocation, expiry, owner existence and enabled flag. `last_used_at` is
written at most once per minute per token per node. The user name of a Basic
credential is ignored (`twine` sends `__token__`). The principal carries the token expiry
(`expires_at`), the scopes, the token id and the owner's e-mail and display name.

## Internal JWT (`internal_jwt`)

EdDSA JWT with mandatory `kid`; claims `iss`, `sub` (user id), `tid` (a UUID), `iat`, `exp`,
plus an optional group snapshot `grp` and `gat` (confirmation time). Verification
order: algorithm, `kid`, signature, then (only for validly signed tokens) issuer,
lifetime, expiry, account state, `iat >= users.tokens_valid_after`, revoked `tid`.
`exp - iat` may not exceed 8 hours even for a correctly signed token. Time is read
from the injected `Clock`, not from the JWT library. The principal carries the session `exp` as
`expires_at`.

Key ring: one signing key plus up to 8 verify-only keys, selected by `kid`. Rotation:
deploy the new key as current with the old one listed as previous, wait one session
lifetime (8 hours), then drop the old one. `InternalJwtService::replace_ring` swaps the
ring without a restart. Keys load from PKCS#8 PEM (private) and SPKI or PKCS#8 PEM
(previous).

Single session revocation goes through the shared `RevokedTokenRepo` port of `rustryx-core`
(table `revoked_tokens`, one list for every node). The list is keyed by `Uuid`, so a `tid` that is
not a UUID is refused on both sides (`revoke` and `verify`), and the hyphenated, upper case and urn
spellings of one id are one revocation. `MemRevokedTokens` is the in-process double for tests and
single node use; it is invisible to other nodes. Revoking every session of a user moves
`users.tokens_valid_after`.

## Login guard (`login_guard`)

Counters live in `LoginAttemptRepo` (PostgreSQL), per `(account, ip)` and per account. The account is
the typed name, case folded, whether or not it exists. After 5 failures a counter locks for 1 minute,
doubling per further failure up to 30 minutes. Success resets both counters (and costs no write when
nothing was counted). When the repository is unreachable, password logins fail closed with
`GuardUnavailable`.

## Group mapping (`groups`)

`RoleMapper` serves both providers. Order: `admin_groups` (exact), `idp_group_mappings`
rows, optional auto-mapping (off), then `default_role`, otherwise the identity is denied.

- Matching is exact and case sensitive on the whole value: no trimming, no substring,
  no case folding. `/rustryx/admins2`, `/Rustryx/Admins` and `rustryx/admins` do not
  match `/rustryx/admins`.
- `nx-admin` is reachable only through `admin_groups`. A table row that points at
  `nx-admin` is ignored, auto-mapping skips `nx-admin` and `nx-anonymous`, and
  `default_role` may be neither.
- `manual` roles are added to external ones only when `allow_local_role_supplement` is
  true, and never `nx-admin`.
- Unknown groups are ignored with a debug log and `rustryx_auth_unknown_groups_total`.

`JitProvisioner` modes: `jit`, `allowlist` (exact usernames), `manual`. OIDC accounts
are found only by `(issuer, subject)`; a username used by another identity is
`UsernameConflict` (409) and is never linked by name or e-mail. A new account stores the e-mail and
display name the principal carries. External roles are persisted at most once per 5 minutes per user
per node.

## Group freshness

Every age decision is made from `ExternalGroups::confirmed_at`, the time the PROVIDER confirmed the
membership, never from the time the chain received the answer. A group set is usable while it is at
most 5 minutes old at `now` and does not lie in the future beyond a 5 second clock skew (an unaged
set must not grant anything). The chain keeps the newest confirmed LDAP set per user; when the
resolver fails it serves that copy while it is within the window, counted from its own confirmation.
The total staleness is therefore at most 5 minutes from the directory's answer whichever layer serves
it: the 60 second cache of the LDAP resolver, the chain's copy or the principal cache (whose entry
ends at `confirmed_at + 5 minutes`). Tests: `chain/tests_window.rs`.

## Metrics

`telemetry.rs` owns every name and label value; the series are those of docs/slo.md and
`deploy/monitoring/rustryx-alerts.yaml`.

| series | type | labels | emitted by |
|---|---|---|---|
| `auth_attempts_total` | counter | `method` (`local`, `ldap`, `token`, `internal_jwt`, `oidc`, `robot`, `scan`, `unknown`), `result` (`ok`, `denied`, `error`) | `AuthService`, once per non anonymous authentication |
| `oidc_requests_total` | counter | `op` (`jwks`, `discovery`), `result` (`ok`, `error`, `timeout`) | every fetch of Keycloak |
| `oidc_jwks_age_seconds` | gauge | none | the OIDC background task; 0 after a good fetch, otherwise growing, counted from the start while there was never a good one |
| `ldap_requests_total` | counter | `result` (`ok`, `error`, `timeout`) | service connection opens, searches and user binds; a refused password and `noSuchObject` are `ok` |
| `auth_breakglass_logins_total` | counter | none | a password verification of `AuthServiceConfig::breakglass_username` (cache hits are not new logins) |

`auth_attempts_total` is also counted by the HTTP middleware today. Exactly one of the two may count,
or every request counts twice: set `AuthServiceConfig::record_attempt_metrics` to `false` when the
middleware keeps counting (the label vocabulary is the same, so the dashboards do not change when the
emission moves).

## Wiring

```rust
let gate = Arc::new(BlockingGate::new(GateConfig::default()));
let passwords = Arc::new(Passwords::new(PasswordConfig::recommended(), gate.clone())?);
// `revoked_tokens` is the PostgreSQL implementation of `RevokedTokenRepo` (one list for all nodes).
let jwt = Arc::new(InternalJwtService::new(key_ring, InternalJwtConfig::default(),
    clock.clone(), users.clone(), revoked_tokens));
let mapper = Arc::new(RoleMapper::new(idp_mappings, roles, mapper_config));
let jit = Arc::new(JitProvisioner::new(users.clone(), clock.clone(), JitConfig::default()));
let guard = Arc::new(LoginGuard::new(login_attempts, clock.clone(), GuardPolicy::default()));
let service = Arc::new(AuthService::new(
    AuthServiceDeps {
        users: users.clone(),
        clock: clock.clone(),
        local_basic: Arc::new(LocalPasswordAuthenticator::new(users.clone(), passwords.clone())),
        api_token: Arc::new(TokenAuthenticator::new(tokens, users.clone(), clock.clone())),
        internal_jwt: Arc::new(InternalJwtAuthenticator::new(jwt.clone())),
        oidc: Some(ExternalIdp { authenticator: oidc_authenticator, resolver: oidc_resolver }),
        ldap: Some(ExternalIdp { authenticator: ldap_authenticator, resolver: ldap_resolver }),
        guard, passwords: passwords.clone(), mapper, jit,
    },
    AuthServiceConfig {
        // `auth.bootstrap_admin.username`: its password logins raise `auth_breakglass_logins_total`.
        breakglass_username: Some(config.auth.bootstrap_admin.username.clone()),
        // The HTTP middleware counts attempts today; keep exactly one counter.
        record_attempt_metrics: false,
        ..AuthServiceConfig::default()
    },
));
// Supervised task: keeps cached principals, remembered passwords and mapping tables in step with
// other nodes.
join_set.spawn(service.invalidation_task(bus.as_ref(), shutdown.clone()));
```

The HTTP layer builds the context of each request with
`AuthContext::new().with_client_ip(ip).with_request_id(id).with_deadline(request_deadline)` and calls
`authenticate_detailed`, so a refusal is counted with its method.

`Passwords::new` hashes once to prepare the dummy hash (about 100 ms at the defaults):
build it at startup.

## Decisions

**ADR-1. Principal cache.** Keyed by SHA-256 of the bearer, capacity bounded, lifetime
`min(principal.expires_at, 5 minutes, group confirmation + 5 minutes)`, dropped on invalidation
events. A principal whose credential carries no expiry is not cached. `ryx_*` tokens are not cached: a
leaked token must die on the next request after revocation, and the lookup is one indexed read.

**ADR-2. Group snapshot in the session token.** A UI request carries no Keycloak access
token, and persisted rows grant nothing, so the internal JWT carries the groups the login
was confirmed with (`grp`, `gat`). The chain refuses a snapshot older than 5 minutes
(clamped, a larger configured value is ignored), so the UI session layer must reissue the
token whenever it refreshes the Keycloak token. A session token without a snapshot for an
OIDC user is refused, never treated as "no groups, default role".

**ADR-3. API tokens of OIDC users.** Group membership cannot be confirmed for a long lived
token, so such a token carries no OIDC derived roles. It gets `manual` rows only with
`allow_local_role_supplement`. LDAP owners are different: the directory is asked on every
request.

**ADR-4. `nx-admin` has one door.** See "Group mapping". The audited setting
(`admin_groups`) is the only way in; the editable table is not.

**ADR-5. The guard covers password credentials only.** API tokens have 256 bits of
entropy, so counting their failures protects nothing and would let a stale CI token lock a
person out. Blank credentials are refused before the guard.

**ADR-6. Gate semantics.** See `BlockingGate`: bounded waiters, permit follows the job, the request
deadline bounds the wait.

**ADR-7. The `ryx_` prefix is the token namespace on Basic.** A Basic password that starts
with it goes to the token authenticator and never to the password check, so
`validate_new_password` refuses such passwords.

**ADR-8. Denial reporting.** `AuthError::AccessDenied` (Core revision 2, gap 20) is the answer for
a principal that maps to no role and has no default role, and for an identity that may not be
provisioned (`manual` or `allowlist` mode). `GroupLookupFailed` is left for what it says: a lookup
that failed. `AccountDisabled` is left for a disabled account. Both carry a reason in the log.

**ADR-9. Rehash is a compare and set.** The upgrade write is `UserRepo::set_password_hash_if` on the
hash that was verified (gap 24 closed). If someone changed the password in the meantime the swap does
not happen and their change wins; a store error is logged and never fails the login.

**ADR-10. The revocation list is a core port.** `RevokedTokenRepo` (gap 21) replaces the crate-local
trait. `tid` is a UUID on both sides. The in-memory implementation stays as a test double.

**ADR-11. Basic passwords are cached for 60 seconds, by HMAC, behind a row check.** See "The cache of
verified Basic passwords". The alternatives were an unkeyed hash of the credentials (precomputable
from a heap dump) and an entry trusted until an event arrives (a missed event would keep a changed
password alive); both were rejected.

**ADR-12. Confirmation time over receipt time.** `ExternalGroups::confirmed_at` (gap 23 closed) is the
clock of every age decision, so the layers of caching cannot add up beyond 5 minutes. The OIDC resolver
reports the confirmation the authenticator stamped on the verified token and fails closed when a
principal carries none; the LDAP resolver reports the directory's answer time, also for a cached set.

**ADR-13. Profile fields travel on the principal.** `email`, `display_name`, `external_id` and
`expires_at` (gaps 16 and 25 closed): a new account stores the e-mail and name, the principal cache is
bounded by `expires_at` instead of a second parse of the JWT, and the LDAP resolver gets the user DN
as a hint (see docs/ldap.md). None of them decides anything or links an account.
