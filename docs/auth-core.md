# Authentication core (`rustryx-auth`)

This document covers the provider independent half of `rustryx-auth`: local
passwords, API tokens, the internal session JWT, the login guard, the group to role
mapping, just-in-time provisioning and the chain that ties them together. The
Keycloak and LDAP modules implement `Authenticator` and `GroupResolver` and are
described elsewhere.

The crate answers "who is calling and which external groups do they have". It never
decides what they may do: that is `rustryx-rbac`.

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
- just-in-time provisioning of external accounts;
- group resolution, the shared group to role mapping and persistence of external roles;
- the principal cache for JWT bearers.

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

A missing or empty OIDC `groups` claim removes OIDC-origin roles for that request.
An LDAP lookup that fails reuses the previous successful result only while it is at
most 5 minutes old; after that the request is refused. A failed lookup is never an
empty group list.

## Passwords (`passwords`)

- New hashes: argon2id v19, 16 byte random salt, PHC strings. Defaults `m = 64 MiB,
  t = 3, p = 1`. `PasswordConfig::validated` refuses anything below the OWASP floor
  (19 MiB, t = 2, p = 1). Peak memory is `password permits x m`: 8 cores x 64 MiB =
  512 MiB, size the pod for it or lower `m` toward the floor.
- Imported `$2a$`, `$2b$` and `$2y$` bcrypt hashes verify; `$2x$` and argon2i/argon2d do not.
  Every bcrypt hash and every argon2id hash below the configured parameters reports
  `needs_rehash`; `LocalPasswordAuthenticator` then rewrites it with
  `UserRepo::set_password_hash`. Parameters are only raised, never lowered.
- Blank (empty or whitespace only) and oversized (> 1024 bytes) passwords are refused
  before any work: no hash, no gate permit, no database read.
- `Passwords::verify_dummy` runs a real verification with the configured parameters
  under the same semaphore, for credentials that name no account.
- `validate_new_password`: 12 to 1024 bytes, not blank, must not start with `ryx_`
  (see ADR-7). The offline list of compromised passwords is applied by the API layer.

### `BlockingGate`

Two lanes, each a `tokio::sync::Semaphore`: passwords (physical cores) and general
(2 x logical CPUs). Invariants:

1. the permit is taken before `spawn_blocking`;
2. the permit moves into the job and is released when the job ends, not when the
   caller stops waiting (a cancelled caller leaves the CPU busy);
3. waiters are bounded too: past `max_waiting` the call fails at once, and no waiter
   lives longer than `permit_wait`.

No permit in time gives `AuthError::Overloaded`, which the HTTP layer maps to 503 with
`Retry-After`.

## API tokens (`tokens`)

`ryx_` + 32 CSPRNG bytes as base64url (47 characters). Only the SHA-256 hex is
stored; the secret is returned once by `create_token`. Lifetime is mandatory:
90 days by default, 365 days at most. The authenticator checks format, looks the
hash up through `TokenRepo`, compares the stored and computed hash in constant time,
then checks revocation, expiry, owner existence and enabled flag. `last_used_at` is
written at most once per minute per token per node. The user name of a Basic
credential is ignored (`twine` sends `__token__`).

## Internal JWT (`internal_jwt`)

EdDSA JWT with mandatory `kid`; claims `iss`, `sub` (user id), `tid`, `iat`, `exp`,
plus an optional group snapshot `grp` and `gat` (confirmation time). Verification
order: algorithm, `kid`, signature, then (only for validly signed tokens) issuer,
lifetime, expiry, account state, `iat >= users.tokens_valid_after`, revoked `tid`.
`exp - iat` may not exceed 8 hours even for a correctly signed token. Time is read
from the injected `Clock`, not from the JWT library.

Key ring: one signing key plus up to 8 verify-only keys, selected by `kid`. Rotation:
deploy the new key as current with the old one listed as previous, wait one session
lifetime (8 hours), then drop the old one. `InternalJwtService::replace_ring` swaps the
ring without a restart. Keys load from PKCS#8 PEM (private) and SPKI or PKCS#8 PEM
(previous).

Single session revocation uses a shared list of `tid` (`RevokedTokenStore`).
Revoking every session of a user moves `users.tokens_valid_after`.

## Login guard (`login_guard`)

Counters live in `LoginAttemptRepo` (PostgreSQL), per `(account, ip)` and per
account. The account is the typed name, case folded, whether or not it exists. After 5
failures a counter locks for 1 minute, doubling per further failure up to 30 minutes.
Success resets both counters (and costs no write when nothing was counted). When the
repository is unreachable, password logins fail closed with `GuardUnavailable`.

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
`UsernameConflict` (409) and is never linked by name or e-mail. External roles are
persisted at most once per 5 minutes per user per node.

## Wiring

```rust
let gate = Arc::new(BlockingGate::new(GateConfig::default()));
let passwords = Arc::new(Passwords::new(PasswordConfig::recommended(), gate.clone())?);
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
    AuthServiceConfig::default(),
));
// Supervised task: keeps cached principals and mapping tables in step with other nodes.
join_set.spawn(service.invalidation_task(bus.as_ref(), shutdown.clone()));
```

`Passwords::new` hashes once to prepare the dummy hash (about 100 ms at the defaults):
build it at startup.

## Decisions

**ADR-1. Principal cache.** Keyed by SHA-256 of the bearer, capacity bounded, lifetime
`min(token exp, 5 minutes, group confirmation + 5 minutes)`, dropped on invalidation
events. `ryx_*` tokens are not cached: a leaked token must die on the next request after
revocation, and the lookup is one indexed read.

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

**ADR-6. Gate semantics.** See `BlockingGate`: bounded waiters, permit follows the job.

**ADR-7. The `ryx_` prefix is the token namespace on Basic.** A Basic password that starts
with it goes to the token authenticator and never to the password check, so
`validate_new_password` refuses such passwords.

**ADR-8. Denial reporting.** `AuthError` has no "access denied" variant. A principal that
maps to no role and has no default role is reported as `GroupLookupFailed` (403 class), and
an identity that may not be provisioned (`manual` or `allowlist` mode) as `AccountDisabled`.
Both carry a reason in the log. See "core gaps" in the work report.

**ADR-9. Rehash is best effort.** The upgrade write cannot be a compare-and-set with the
current port, so the row is re-read first and the write skipped when the hash changed during
the login. A CAS in `UserRepo` would close the remaining window.

**ADR-10. Revocation list lives here for now.** `RevokedTokenStore` is defined in
`internal_jwt` because `rustryx-core` has no port for it. Moving it to core changes only
the import path.
