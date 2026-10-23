# OIDC (Keycloak)

Code: `crates/rustryx-auth/src/oidc/`. Normative text: README sections 5, 8.3 and 8.5 of the
specification. This page says how each rule is enforced and where its test lives.

The module answers "who is calling" and "which external groups does that identity have". It
never decides what the caller may do: roles come from `rustryx-rbac` and the shared group
mapping.

## Wiring

```rust
let provider = OidcProvider::new(config)?;           // validates, never contacts Keycloak
let background = provider.spawn_background(shutdown); // discovery retry, JWKS refresh
let authenticator = provider.bearer_authenticator();  // impl Authenticator
let resolver = provider.group_resolver();             // impl GroupResolver
let flow = provider.login_flow();                     // Option<OidcLoginFlow>, None without `ui`
let status = provider.status();                       // flags and counters for metrics
```

Handlers of `rustryx-api` use the flow like this:

- `GET /login`: `flow.start()?` gives `authorization_url` (302 `Location`) and `set_cookie`
  (a complete `Set-Cookie` header value).
- callback: read the cookie named `flow.cookie_name()`, call
  `flow.complete(code, state, cookie).await`, always send `flow.clear_cookie()`, then issue the
  internal JWT from the returned `OidcIdentity`.
- error mapping: `InvalidToken` (cookie, state, ID token) is a 400, `InvalidCredentials`
  (Keycloak refused the code) a 401, `ProviderUnavailable` a 503.

For just-in-time provisioning the `Principal` has no email field. Call
`OidcBearerAuthenticator::validate(token)` (returns `VerifiedBearer { identity, expires_at }`)
when the email or the token expiry is needed; `authenticate` stays the chain entry point.

## Configuration

`OidcConfig::new(issuer_url, audience)` sets the normative defaults; `validate()` runs inside
`OidcProvider::new` and lists every problem at once. Out-of-range values are rejected, never
clamped.

| Field | Default | Rule |
|---|---|---|
| `issuer_url` | required | HTTPS only. Realm URL the node calls (discovery). |
| `public_issuer_url` | none | Split horizon. When set, `iss` must equal this exact string. |
| `audience` | required | Product client id that `aud` must contain. `account` is refused. |
| `authorized_parties` | empty | Accepted `azp` values; empty means only `audience`. |
| `clock_skew` | 60 s | At most 60 s. Applies to `exp`, `nbf`, `iat`. |
| `jwks_ttl` | 10 min | 30 s to 1 h. `Cache-Control: max-age` wins, clamped to the same range. |
| `jwks_min_refresh_interval` | 30 s | At least 30 s. Window of forced refreshes (unknown `kid`). |
| `discovery_refresh` / `discovery_retry` | 1 h / 15 s | Background loop periods. |
| `connect_timeout` / `request_timeout` | 3 s / 5 s | Every outgoing call to Keycloak. |
| `groups_claim` | `groups` | Top-level claim with the group paths. Role claims and claims read for other purposes are refused. |
| `max_groups` / `max_group_bytes` | 256 / 512 | Caps on the groups claim. |
| `max_token_bytes` | 32 KiB | Longer bearer values are "not ours". |
| `extra_root_certs_pem` | empty | Corporate CA, merged with the platform roots. |
| `ui` | none | `UiClientConfig`: confidential client `rustryx-ui`, redirect URL, cookie key. |

`UiClientConfig`: `client_secret` must be non-empty (public clients do not exist here),
`redirect_url` is an exact https URL (http only for loopback, no wildcard), `cookie_key` is 32
bytes as 64 hex characters or base64 (all-equal keys are refused), `flow_ttl` is at most 10
minutes, `scopes` (default `profile`, `email`; `openid` is implicit) must not contain
`offline_access`.

Mapping from `rustryx-config` (`auth.oidc.*`):

| `rustryx-config` | `OidcConfig` |
|---|---|
| `issuer` | `issuer_url` (the URL the node calls) |
| `public_issuer_url` | `public_issuer_url` (what tokens carry as `iss` and browsers open) |
| `client_id` | `audience` and `ui.client_id` |
| `client_secret` | `ui.client_secret` |
| `groups_claim` | `groups_claim` |
| `scopes` | `ui.scopes` |
| `clock_skew_secs` | `clock_skew` |
| `auth.cookie_key` | `ui.cookie_key` |
| `server.base_url` + callback path | `ui.redirect_url` |

`authorized_parties` has no key in `rustryx-config` yet. Tokens of the CLI client carry
`azp: rustryx-cli`, so without it they are refused by the `azp` rule.

Keycloak must be reachable over HTTPS. There is no plaintext switch: the JWKS decides who may
sign tokens. A private CA goes into `extra_root_certs_pem` or the platform trust store.

## Rules and where they are enforced

| Rule | Enforced in | Test (`oidc/tests/`, unit tests) |
|---|---|---|
| Startup does not fail when Keycloak is down; state is a flag | `OidcProvider::new`, `Discovery`, `OidcStatus` | `startup_without_keycloak_is_a_state_not_an_error`, `background_task_recovers_when_keycloak_appears` |
| `iss` equals the configured issuer exactly | `TokenValidator::peek` and `Validation::set_issuer` | `must_not_accept_wrong_issuer` |
| `aud` contains the client id; `account` alone does not authorize | `Validation::set_audience`, `OidcConfig::validate` | `must_not_accept_wrong_audience_or_account_alone` |
| `azp` equals the client when present | `check_access_token` | `must_not_accept_authorized_party_other_than_the_client` |
| `exp` required, `nbf`/`iat` with 60 s skew | `Validation`, `check_issued_at` | `must_not_accept_expired_token_but_tolerates_the_skew`, `must_not_accept_future_nbf_beyond_skew`, `must_not_accept_future_iat_beyond_skew`, `must_not_accept_token_without_exp` |
| Only RS256 and ES256; `none` and HS* never verified | `peek`, `parse_jwks` | `must_not_accept_alg_none`, `must_not_accept_hs256_signed_with_the_public_key_as_secret`, `must_not_accept_algorithms_outside_the_allowlist`, `must_not_accept_key_published_for_another_algorithm` |
| `kid` required | `verify` | `must_not_accept_token_without_kid` |
| `jku`, `x5u`, `x5c`, `jwk` headers ignored | never read; keys only from the JWKS | `must_not_follow_jku_x5u_or_jwk_headers` |
| Keys only from an HTTPS JWKS with certificate verification | `OidcHttp` (`https_only`), `Discovery::backchannel_url` | `must_not_trust_jwks_from_an_untrusted_certificate`, `must_not_accept_a_jwks_uri_over_plain_http`, `must_not_accept_plain_http_issuer` |
| Unknown `kid`: at most one refresh per 30 s per issuer, singleflight | `JwksCache::claim` | `must_not_refresh_jwks_more_than_once_per_window_for_unknown_kid`, `must_not_refresh_more_than_once_per_window_under_concurrent_unknown_kids` |
| A vanished key is not accepted | snapshot replaced as a whole | `must_not_accept_a_key_that_vanished_from_the_last_jwks` |
| Outage: last in-memory JWKS, `exp`/`iss`/`aud`/signature checked every time | `JwksCache::fetch_and_store` keeps the snapshot | `keycloak_outage_uses_cached_jwks_but_still_checks_expiry`, `unknown_kid_during_outage_is_a_401_not_a_503` |
| ID token as Bearer rejected | `typ` claim | `must_not_accept_id_token_as_bearer` |
| `offline_access` and offline tokens rejected | `scope`, `typ`, token response scope | `must_not_accept_offline_or_other_token_kinds`, `must_not_accept_offline_scope_in_the_token_response` |
| Role claims ignored; `groups` exact, capped, empty means no groups | `claims.rs` | `must_not_derive_groups_from_realm_or_client_roles`, `groups_claim_is_carried_byte_for_byte`, `caps_group_count_and_length_and_counts_it` |
| Sealed cookie: tampered, expired, replayed, foreign rejected | `SealedCookie`, `OidcLoginFlow::complete` | `must_not_open_tampered_value`, `must_not_accept_an_expired_cookie`, `must_not_accept_a_replayed_callback`, `must_not_accept_a_cookie_sealed_under_another_key_or_name` |
| `state` mismatch rejected | `complete` | `must_not_accept_state_mismatch` |
| ID token: nonce, iss, aud, exp, signature | `validate_id_token` | `must_not_accept_a_forged_or_misdirected_id_token` |
| Login while Keycloak is down: 503, nothing else affected | `Discovery::reachable`, `OidcLoginFlow::start` | `new_logins_get_503_while_keycloak_is_unreachable_and_nothing_else_breaks` |
| Secrets never in `Debug` | `Secret`, manual `Debug` impls | `debug_never_shows_secrets`, `flow_debug_output_hides_secrets`, `debug_never_shows_key_material` |

The integration test `crates/rustryx-auth/tests/oidc_keycloak.rs` runs the same checks against a
real Keycloak 26 with the realm of `deploy/compose/keycloak/realm-rustryx.json`. It needs Docker
and runs only with `RUSTRYX_IT=1`; otherwise it prints why it was skipped and passes.

## Discovery and split horizon

Discovery is fetched from `{issuer_url}/.well-known/openid-configuration` at start (in the
background), then every `discovery_refresh`. The document's `issuer` must equal the expected
issuer exactly, otherwise it is refused and the previous endpoints stay in use.

With `public_issuer_url` the realm has two names. Endpoints under one realm URL are rewritten
to the other where that makes them usable: the authorization endpoint (opened by the browser)
to the public host, the token endpoint and `jwks_uri` (called by the node) to `issuer_url`.
Keycloak with `KC_HOSTNAME` and `KC_HOSTNAME_BACKCHANNEL_DYNAMIC=true` already advertises the
right hosts; the rewrite makes the node correct when it does not.

## JWKS cache

`ArcSwap` snapshot of `kid -> (algorithm, key)`. Only RS256 keys of at least 2048 bits and
ES256 keys on P-256 are kept; encryption keys, `oct` keys, other curves and duplicate `kid`
are skipped and counted. A fetch with no usable key is a failure and keeps the old snapshot.

Unknown `kid`: the first caller of a window takes the right to fetch with one compare-and-swap
(the same step publishes the refresh in flight), the fetch runs in a detached task, callers
that lose the claim wait for that refresh (bounded by the request timeout) and then answer from
the new snapshot. Nobody else fetches in the window. Background refresh and forced refresh
share the window, so Keycloak sees at most one JWKS request per window from request traffic.

## Cookie format

`v1.` + base64url(nonce(12) || AES-256-GCM(ciphertext || tag)). The plaintext is
`{"exp": <unix seconds>, "data": {state, nonce, verifier}}`. The associated data is a fixed label
plus the cookie name. Attributes: `HttpOnly; Secure; SameSite=Lax`, `Max-Age` equal to
`flow_ttl`, `Path` equal to the callback path. `Lax` because the callback is a cross-site
top-level navigation from Keycloak.

Single use: `OidcLoginFlow` remembers used `state` values for the cookie lifetime (bounded
queue, per node). Across nodes the authoritative guard is the one-time authorization code at
Keycloak plus the PKCE verifier that exists only in the cookie.

## Status and metrics

`OidcProvider::status()` returns `OidcStatus`: `discovery_loaded`, `reachable`,
`discovery_failures`, `discovery_last_success`, `jwks_keys`, `jwks_age` (source of
`oidc_jwks_age_seconds`), `jwks_refresh_ok`, `jwks_refresh_err`, `jwks_forced_refreshes`,
`jwks_forced_throttled`, `jwks_keys_skipped`, `groups_truncated`, `groups_malformed`.
None of it gates `/readyz`.

## Decisions

1. One HTTP client. `openidconnect` 4 brings `reqwest` 0.12 through `oauth2` (its default
   features), next to the workspace's `reqwest` 0.13. The code flow runs through an
   `AsyncHttpClient` adapter over the 0.13 client (`oidc/http.rs`), so there is one TLS
   configuration: HTTPS only, no redirects, hard timeouts, capped bodies. The 0.12 adapter is
   never called. To remove 0.12 from the graph, set `openidconnect = { version = "4",
   default-features = false }` in the root manifest; the module compiles without those features.
2. Discovery types, the authorization URL, PKCE and the token exchange come from
   `openidconnect`; the ID token is verified by the same `jsonwebtoken` pipeline as access
   tokens. One allowlist, one skew, one JWKS trust path. The library verifier checks `exp`
   without skew and accepts more algorithms than the specification allows.
3. Losers of the refresh claim wait for the running refresh instead of answering 401 at once.
   Keycloak load is the same (one fetch); a key rotation does not turn a burst of requests
   into a burst of 401.
4. `iss` is compared as the configured string, never as a parsed URL: no trailing slash or
   case normalisation can widen what is accepted.
5. Access tokens must carry `typ: Bearer`. Keycloak always sets it; `ID`, `Refresh`, `Offline`
   and a missing claim are refused.
6. `authenticate` returns `Ok(None)` for anything that is not a JWT with RS256 or ES256 and the
   configured `iss` (API tokens, internal JWTs, foreign issuers, `none`, HS*), so the chain moves
   on. The chain must answer 401 when no authenticator claims a Bearer value.
7. Keycloak is only ever contacted over HTTPS, with no plaintext flag.

## Realm requirements

- Audience mapper on every client that obtains tokens for the API: `aud` must contain
  `OidcConfig::audience`. For the compose realm that is `rustryx-ui`, and
  `authorized_parties` must list `rustryx-ui` and `rustryx-cli` because CLI tokens carry
  `azp: rustryx-cli`.
- Group Membership mapper with Full group path on, claim name `groups`, on access and ID tokens.
- Scope `offline_access` is not offered to the clients of the product.
- Access token lifetime 5 minutes; the product session limits (30 minutes idle, 8 hours
  absolute) belong to the internal JWT, not to this module.

## Outage runbook (SRE-28)

- Keycloak down, nodes running: bearer tokens with a known `kid` keep working until their `exp`;
  new interactive logins answer 503; local users, `ryx_*` tokens and break-glass are not touched.
  `oidc_jwks_age_seconds` grows.
- Node restarted during the outage: no JWKS in memory, bearer tokens of this issuer answer 503
  until the background loop reaches Keycloak (every `discovery_retry`).
- After recovery the first successful contact (background loop or any JWKS fetch) makes
  `reachable` true again and logins resume without a restart.

## Open points

- `Principal` has no email or display name; see "Wiring".
- Back-channel logout is out of phase 1 scope; the 5 minute access token bounds the exposure.
- The replay memory of used `state` values is per node.
