# Local development

The stack in `deploy/compose/` gives a developer PostgreSQL 16, Redis 7, SeaweedFS,
Keycloak and OpenLDAP with the same fixtures the integration tests use. Everything
in it is test data with public credentials: never expose the ports beyond the
machine (they are bound to `127.0.0.1`) and never reuse a value from it.

Pipeline and release: [ci-and-release.md](ci-and-release.md). Supply chain:
[supply-chain.md](supply-chain.md).

## Quick start

```
cd deploy/compose
docker compose up -d          # backing services
docker compose ps             # all healthy; seaweedfs-init and certs show "Exited (0)", that is correct
```

No `.env` is needed: `docker-compose.yml` loads `services.yaml` with the values of
`.env.example` (one file for every value). To change one value for a run, set it in
the shell: `POSTGRES_PASSWORD=other docker compose up -d`.

```
docker compose --profile rustryx up --build   # also builds and runs rustryx
docker compose down -v                        # stop and delete the data
```

`docker compose up -d --wait` reports the one-shot `seaweedfs-init` job as exited and
returns an error even though it succeeded; use `up -d` and `ps`, or start named
services (`up -d --wait keycloak ldap`, which is what `ci/verify-fixtures.sh` does).

## Services

| Service | Host address | Credentials (from `.env.example`) |
|---|---|---|
| PostgreSQL 16 | `127.0.0.1:5432`, database `rustryx` | `rustryx` / `rustryx-test-only` |
| Redis 7 | `127.0.0.1:6379` | password `redis-test-only` |
| SeaweedFS (S3) | API `127.0.0.1:8333`, filer `127.0.0.1:8888`, bucket `rustryx-blobs` | `rustryx-seaweed` / `seaweed-test-only-secret` |
| Keycloak | `https://localhost:8443` (realm `rustryx`) | admin console `admin` / `admin-test-only` |
| OpenLDAP | `ldaps://localhost:1636`, `ldap://localhost:1389` (StartTLS) | admin `cn=admin,dc=rustryx,dc=test` / `ldap-admin-test-only` |
| rustryx (profile) | `http://localhost:8081` | bootstrap admin `admin` / `admin-test-only-0123456789` |

The bucket is created by the `seaweedfs-init` job (a second run treats "already exists" as success). Keycloak is `start-dev`
with `--import-realm`; OpenLDAP loads its seed before it opens its sockets, so
"healthy" means "seeded".

Notes that cost time once:

* Blobs go to SeaweedFS (`chrislusf/seaweedfs:4.13`), one process with the S3 API
  on port 8333. It is for local use only; any S3 endpoint works instead.
* Keycloak's issuer is `https://localhost:8443/realms/rustryx` whichever address a
  client used (`KC_HOSTNAME`). From a container, reach it as `https://keycloak:8443`
  and set the public issuer (`RUSTRYX_AUTH_OIDC_PUBLIC_ISSUER_URL`) to the localhost
  form. The listener port is part of the issuer; change `KEYCLOAK_HTTPS_PORT` only
  together with every client that validates `iss`.
* The management port of Keycloak (9000) is plain HTTP inside the network, for the
  healthcheck; it is not published.

## TLS

The `certs` service writes a throwaway CA and one server certificate to
`deploy/compose/certs/` (git-ignored) on the first start and renews them when less
than 30 days remain. The certificate lists `localhost`, `ldap`, `keycloak`, `seaweedfs`,
`rustryx`, `rustryx.localhost`, `127.0.0.1` and `::1`. The CA private key is deleted
right after signing. Trust it with `ca.crt` from that directory:

```
curl --cacert deploy/compose/certs/ca.crt https://localhost:8443/realms/rustryx/.well-known/openid-configuration
```

`SSL_CERT_FILE` or `auth.ldap.ca_file` point at it; a custom CA is mounted, never
baked into an image. Delete the directory to rotate.

## The shared fixtures

`deploy/compose/keycloak/realm-rustryx.json` and `deploy/compose/ldap/seed.ldif` are
the fixtures of both the compose stack and the integration tests (README 8.6). Change
them in one place and run `ci/verify-fixtures.sh`.

### Keycloak realm `rustryx`

* Token lifetimes: access 300 s, SSO idle 1800 s, SSO max 36000 s, offline sessions
  disabled.
* Client `rustryx-ui`: confidential, authorization code with PKCE S256 only, exact
  redirect URIs (`http://localhost:8081/api/v1/auth/oidc/callback` and
  `https://rustryx.localhost/api/v1/auth/oidc/callback`; the path is the one assumed
  for the API, change it with the router), no implicit, no password grant,
  `offline_access` not among its scopes. Secret `rustryx-ui-secret-TEST-ONLY`.
* Client `rustryx-cli`: confidential, service account (client credentials), device
  authorization allowed, no public client. Secret `rustryx-cli-secret-TEST-ONLY`.
* Both clients carry an Audience mapper for their own client id. `rustryx-cli` also
  carries one for `rustryx-ui`, because the product requires `aud` to contain the UI
  client id (`auth.oidc.client_id`); without it a CI token could never pass.
* A Group Membership mapper with **Full group path** on writes the claim `groups` to
  the access and ID token.

| User | Password | `groups` claim |
|---|---|---|
| `alice` | `alice-test-only` | `/rustryx/admins` |
| `bob` | `bob-test-only` | `/rustryx/readers` |
| `carol` | `carol-test-only` | `/rustryx/team-payments/publishers` |
| `dave` | `dave-test-only` | none |
| `erin` | `erin-test-only` | `/rustryx/team-x/admins`, a look-alike of the admin group that must not grant `nx-admin` |
| service account of `rustryx-cli` | client credentials | `/rustryx/ci/demo` |

Limits of an import: the default role keeps the standard `offline_access` composite
(an import cannot change it), so offline tokens are blocked by removing the scope
from the clients and by the product rejecting them, not by the role. New clients
created by hand inherit `offline_access` as an optional scope; remove it there too.
Names must not contain `(` or `)`: Keycloak's user profile rejects them and the user
then stops at a "verify profile" step.

### OpenLDAP seed

Base DN `dc=rustryx,dc=test`, created by the image; `seed.ldif` starts below it, so
it also loads into images that create the base from `LDAP_DOMAIN`. Load it with
`ldapmodify -a` (its last record is a `changetype: modify`).

* Users `alice`, `bob`, `carol`, `dave`, `erin` with password `<login>-test-only`.
* Search account `uid=svc-rustryx,ou=services,...` (password `svc-rustryx-test-only`).
* A user whose login is `star*(paren)\back,comma+plus`, password `special-test-only`:
  it contains every character that needs RFC 4515 escaping in a filter and RFC 4514
  escaping in a DN. It is also a member of `readers`, so the group filter carries the
  escaped DN as well.
* Groups: `rustryx-admins` (alice), `readers` (bob and the special user), a nested
  chain `carol -> chain-l1 -> chain-l2 -> chain-l3 -> chain-l4`, a deliberate cycle
  `cycle-a <-> cycle-b` with `dave` in `cycle-a`, and look-alikes of the admin group
  (`cn=rustryx-admins,ou=decoys` and `cn=team-x-admins`, both with `erin`) that an
  exact DN match must not treat as admin.
* The compose image runs the **memberOf overlay**, which maintains `memberOf` for
  direct membership only; nesting is the client's job. Without the overlay use the
  reverse search (`auth.ldap.group_strategy = member_search`, filter
  `(member=<escaped DN>)`, repeated per level).
* Anonymous bind is refused and an empty password with a DN is refused.
* From inside the container use `ldaps://ldap`, not `ldaps://localhost`: libldap
  canonicalizes `localhost` through a reverse lookup of `::1`, which the certificate
  does not list. Clients on the host (rustls) check `localhost` directly and are fine.

### Verify them

```
ci/verify-fixtures.sh          # boots Keycloak and OpenLDAP, runs ~60 checks, stops them
KEEP_STACK=1 ci/verify-fixtures.sh   # leave the stack running afterwards
```

It checks, among others: alice's access token has `groups` with `/rustryx/admins` and
`aud` with `rustryx-ui`; removing bob from `/rustryx/readers` changes his next token
(and puts him back); password grant, offline scope and an authorization request
without PKCE are refused; an LDAP bind with the right password works and with an
**empty** password is refused. It also fails when the client secrets of the realm
file and of `.env.example` differ. Tokens are obtained through the real authorization
code flow with PKCE, because the realm forbids the password grant.

## Images

```
docker build -f Dockerfile \
  --build-arg VERSION=0.0.0-local --build-arg REVISION=$(git rev-parse HEAD) \
  --build-arg CREATED=$(date -u +%Y-%m-%dT%H:%M:%SZ) -t rustryx:local-glibc .
docker build -f Dockerfile.musl ...same arguments... -t rustryx:local-musl .
ci/test-image.sh rustryx:local-glibc glibc
ci/test-image.sh rustryx:local-musl musl
```

The labels have no defaults on purpose: an image built without the three build
arguments fails `ci/check-image-labels.sh`. `crates/rustryx-server` must declare
`[[bin]] name = "rustryx"`. Debug symbols:
`docker build -f Dockerfile --target debug-symbols --output type=local,dest=out .`.

## Tools you need locally

`docker` with compose, `python3` (3.11 or later), `shellcheck`, and for the supply
chain checks `cargo-deny`, `cargo-audit`, `hadolint`, `actionlint`, `yamllint`.
`ci/install-tool.sh` installs the pinned Linux binaries; on macOS use your package
manager and compare the versions with `ci/tools.lock`.

## Environment variables of the `rustryx` service

Names follow `RUSTRYX_<SECTION>_<KEY>` (`crates/rustryx-config/src/keys.rs`). The
profile sets, among others: `RUSTRYX_ENVIRONMENT=development`,
`RUSTRYX_SERVER_LISTEN_ADDR`, `RUSTRYX_SERVER_BASE_URL`, `RUSTRYX_DATABASE_URL`,
`RUSTRYX_LOCK_BACKEND` and `RUSTRYX_LOCK_REDIS_URL`, `RUSTRYX_BLOB_KIND` and the
`RUSTRYX_BLOB_S3_*` set, `RUSTRYX_AUTH_BOOTSTRAP_ADMIN_*`, `RUSTRYX_AUTH_OIDC_*`
(issuer, public issuer, client id and secret, admin groups), `RUSTRYX_AUTH_LDAP_*`
(URL, CA file, bind DN and password, bases, filter, group strategy, admin groups),
`SSL_CERT_FILE`. The container runs as 10001:10001 with a read-only root, `/tmp` as
tmpfs and all capabilities dropped, like the Helm chart. The binary does not fully
exist yet, so this profile is written for its final shape and has not been run end to
end; the other services have.
