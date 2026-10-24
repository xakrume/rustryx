# rustryx-testserver

An in-process HTTP server for testing format crates with real clients. It mounts the real
`FormatEngine` over the testkit's in-memory ports, uses the real `Rbac` for authorization and
a small token authenticator, and listens on an ephemeral port. A format crate registers its
handler, creates repositories and users, and points `mvn`, `npm`, `pip`, `twine`, `skopeo`,
`docker`, `helm` or `dart pub` at it. Authorization behaves like production (deny by default,
the two anonymous gates, roles, privileges, content selectors, token scopes), so a test that
passes here says something about the format, not about a permissive double.

It is a dev-dependency crate. Nothing in it is for production, and no binary links it.

```toml
[dev-dependencies]
rustryx-testserver.workspace = true
```

Contents:

1. [A first test](#1-a-first-test)
2. [The server](#2-the-server): builder, URLs, accessors, shutdown.
3. [Fixtures](#3-fixtures): repositories, users, roles, selectors.
4. [What the HTTP layer does](#4-what-the-http-layer-does): routes, credentials, the status a refusal gets, challenges.
5. [OCI: `/v2/` and `/v2/token`](#5-oci-v2-and-v2token)
6. [Proxy tests](#6-proxy-tests): `UpstreamStub`, `FakeUpstream`, the SSRF policy.
7. [Real clients](#7-real-clients): gating, the runner, the machine, one example per tool.
8. [Tests of the harness itself](#8-tests-of-the-harness-itself)
9. [Limits and known gaps](#9-limits-and-known-gaps)

## 1. A first test

```rust
use std::sync::Arc;
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, TestServer, http_client};

#[tokio::test(flavor = "multi_thread")]
async fn upload_then_download() {
    let server = TestServer::builder()
        .register(Arc::new(MyFormatHandler::new()))     // the handler under test
        .start()
        .await;
    server.add_repository(NewTestRepo::hosted("things", RepoFormat::Raw)).await;
    let alice = server.add_user("alice", &["nx-developer"]).await;

    let http = http_client();
    let url = format!("{}/a/b.txt", server.repository_url("things"));
    let put = http.put(&url).header("authorization", alice.bearer_header()).body("hi").send().await.unwrap();
    assert_eq!(put.status(), 201);
    let got = http.get(&url).header("authorization", alice.basic_header()).send().await.unwrap();
    assert_eq!(got.text().await.unwrap(), "hi");

    server.shutdown().await;
}
```

Use `#[tokio::test(flavor = "multi_thread")]`: the server, the client and the engine's
background tasks share the runtime, and a streaming test starves on one thread.

All fixture helpers are `async` (the store ports are). `TestServer::builder()`, the `NewTestRepo`
constructors and the `TestPrivilege` constructors are not.

## 2. The server

### 2.1 Builder

`TestServer::builder()` returns a `TestServerBuilder`. Every method returns the builder.

| Method | Default | Effect |
|---|---|---|
| `register(Arc<dyn FormatHandler>)` | none | Registers a handler for every format it declares (the OCI handler declares `docker` and `oci`). A duplicate format makes `start` panic with the registry's message. |
| `bind_all(bool)` | `false` | `false` binds `127.0.0.1`. `true` binds `0.0.0.0`, so a Docker container reaches the server as `host.docker.internal`. |
| `anonymous_enabled(bool)` | `false` | Initial position of the global anonymous switch (`auth.anonymous_enabled`). A repository also needs `allow_anonymous`. |
| `upstream_policy(UpstreamUrlPolicy)` | `loopback_friendly_policy()` | The SSRF policy for proxy upstreams, see [6.3](#63-the-ssrf-policy). |
| `limits(Limits)` | `Limits::default()` | Engine limits. |
| `blob_store(Arc<dyn BlobStore>)` | `MemBlobStore` | Where bytes go. `blobs()` then returns an empty `MemBlobStore`. Use `SyntheticBlobStore` for bodies that must not be held in memory. |
| `upstream_client(Arc<dyn UpstreamClient>)` | reqwest client | Replaces the HTTP client of proxies, for example with a `FakeUpstream`. The URL policy still applies. |
| `executor_config(ExecutorConfig)` | engine default | Timeouts, retries, breaker. The default backs off for seconds; `fast_executor_config()` is three attempts a millisecond apart. |
| `oci_token_ttl(Duration)` | 5 minutes | Lifetime of the tokens `/v2/token` issues. |
| `customize_ports(FnOnce(&mut Ports))` | none | Adjusts the ports before the engine is built: the policy hook, the event sink, the routing gate (the default is the real `RuleRoutingGate` over the store). |
| `start().await` | | Starts the server. Panics with the cause when it cannot (a handler that cannot be registered, a port that cannot be bound). |
| `try_start().await` | | The same, returning `Result<TestServer, HarnessError>`. |

### 2.2 `TestServer`

| Method | Returns |
|---|---|
| `base_url()` | `http://127.0.0.1:PORT` |
| `host_base_url()` | `http://host.docker.internal:PORT` when `bind_all`, otherwise `base_url()` |
| `repository_url(name)` | `<base_url>/repository/<name>` |
| `host_repository_url(name)` | `<host_base_url>/repository/<name>` |
| `localhost_repository_url(name)` | `http://localhost:PORT/repository/<name>`: the URL for pub inside the Dart container ([7.4](#74-the-runner)) |
| `port()` | the port |
| `store()` | `&MemStore`, every metadata port. Go through a trait object for calls whose names are shared across ports (`docs/testkit.md` section 2). |
| `blobs()` | `&MemBlobStore` |
| `engine()` | `&FormatEngine`, for `context()`, `fills_in_flight()`, `running_tasks()` |
| `audit()` | `&MemAuditSink`, what the engine recorded |
| `rbac()` | `&Rbac` |
| `set_anonymous_enabled(bool)` | flips the global switch while running; the next request sees it |
| `settle().await` | waits until no cache fill and no tracked task is running, so a test can look at the cache after a response without racing the fill |
| `shutdown().await` | closes the listener, cancels fills, waits for the engine's tracked tasks |

Dropping a `TestServer` without `shutdown` stops the listener but does not wait for anything.

`http_client()` is a `reqwest::Client` that ignores `HTTP_PROXY` and friends (the server is on
loopback). `rustryx_testserver::reqwest` re-exports the crate so the version matches.
This workspace builds reqwest without its `form` feature: build form bodies with
`url::form_urlencoded`.

## 3. Fixtures

### 3.1 Repositories

```rust
server.add_repository(NewTestRepo::hosted("maven-hosted", RepoFormat::Maven2)).await;
server.add_repository(NewTestRepo::proxy("maven-central", RepoFormat::Maven2, &upstream.base_url())).await;
server.add_repository(NewTestRepo::group("maven-all", RepoFormat::Maven2, &["maven-hosted", "maven-central"])).await;
```

Builder methods on `NewTestRepo`:

| Method | Effect |
|---|---|
| `allow_anonymous(bool)` | The repository's own flag. |
| `write_policy(WritePolicy)` | `Allow` (hosted default), `AllowOnce`, `Deny` (the default for proxy and group, as in the testkit fixtures). |
| `proxy_config(ProxyConfig)` | Replaces the whole proxy configuration, `remote_url` included (freshness, negative cache, concurrency). |
| `online(bool)` | `false` makes reads answer 503. |
| `quota_bytes(u64)` | Repository quota. |
| `format_config(serde_json::Value)` | `Repository::format_config`. |

`add_repository` returns the `Repository` as stored and panics on an invalid or duplicate name;
`try_add_repository` returns `Result<Repository, HarnessError>`.

### 3.2 Users

```rust
let alice = server.add_user("alice", &["nx-developer"]).await;   // TestUser
```

`TestUser { username, password, token }`. The token is `ryx_` and 43 characters of base64url,
the shape of a real API token. A user authenticates in any of these forms:

| Form | Helper |
|---|---|
| `Authorization: Bearer <token>` | `bearer_header()` |
| `Authorization: Basic user:<password>` | `basic_header()` |
| `Authorization: Basic <any name>:<token>` | `token_basic_header()` (npm, pub, maven, `twine`'s `__token__`) |

Roles are named: the built-in `nx-admin` (everything), `nx-developer` (browse, read, write on
every repository, no delete), `nx-anonymous`, or a role from `add_role_with_privileges`. A
user with no roles (`&[]`) is authenticated and holds nothing.

`set_user_roles(&user, &[..])` replaces the roles; the next request sees it (the harness wires
the invalidation bus to `Rbac`, so the privilege cache is not stale for 60 seconds).
`add_scoped_token(&user, &["repo:maven-hosted:read"]) -> String` returns an extra API token
restricted to scopes; a scope list can only narrow the owner's rights.
`try_add_user` returns the error instead of panicking (duplicate name, unknown role).

### 3.3 Roles, privileges, selectors

```rust
use rustryx_testserver::{READ, TestPrivilege};

let selector = server.add_selector("acme-only", r#"coordinate.groupId == "org.acme""#).await;
server.add_role_with_privileges("acme-reader", &[
    TestPrivilege::content_selector("maven-hosted", &selector, &READ),
]).await;
let bob = server.add_user("bob", &["acme-reader"]).await;
```

`add_selector` compiles the expression first (as the admin API does), so a typo fails there.
`TestPrivilege` constructors: `repository_view(repo_or_star, actions)`,
`repository_view_of_format(format, repo_or_star, actions)`,
`content_selector(repo_or_star, &selector, actions)`. The constants `READ`
(browse, read), `READ_WRITE` and `ALL_ACTIONS` save typing. A selector sees `coordinate.*`
only if the handler's `classify` attaches a coordinate; that is the point of testing selectors
here.

## 4. What the HTTP layer does

It is the job of `rustryx-api` and the artifact module of the server, reduced to what a test
needs. It routes `/repository/{name}/{*path}` for every format, builds the `RequestHead`
(method, raw path, query, headers, client address, base URL), authenticates, calls
`FormatEngine::serve` and streams the response body. It never buffers a body.

* **The path below the repository is passed on raw**, still percent-encoded, without a leading
  slash. The format's `classify` decodes it once (SEC-19). An encoded slash reaches `classify`
  as `%2F`.
* **The base URL is built from the `Host` header** of the request, so a document written by a
  format (a tarball URL, an archive URL) points back at the address the client used:
  `127.0.0.1:PORT` from the host, `host.docker.internal:PORT` or `localhost:PORT` from a
  container. Without a `Host` header it is the bound address.
* **Every response carries `x-request-id`** (`test-N`), also in the engine's logs.

### 4.1 Credentials

| Request | Principal |
|---|---|
| no `Authorization` | anonymous |
| a valid Basic (password or `ryx_` token) or Bearer credential | the user, with the roles the store holds at that moment |
| anything present and unusable (unknown scheme, bad base64, empty password, two headers, wrong password, unknown or expired token) | **401, never anonymous**: a client that tried to authenticate is not downgraded |
| the store that holds the roles fails | 503 with `Retry-After`, never "no roles" |

### 4.2 The status of a refusal

The engine decides; the harness adds the challenge.

| Caller | Situation | Status |
|---|---|---|
| anonymous | the switch is off, or the repository flag is off, or the action is a write | 401 |
| authenticated, valid | holds some privilege on the repository, but not this action (or this path, for a selector) | 403 |
| authenticated, valid | holds nothing on the repository | 404, so its existence is not revealed (a handler returning `false` from `hide_inaccessible_repository`, which pub does, gets 403 instead) |
| authenticated, valid | the authorizer's backend failed | 403 (fail closed) |
| any | repository unknown, or an invalid name | 404 |
| any | write or delete on a proxy or group | 405, before the body is read |
| any | repository offline | 503 |

Writes by anonymous callers are refused outright (401) even when both anonymous gates are open.

### 4.3 Challenges

A 401 (and for pub also a 403) that carries no `WWW-Authenticate` of its own gets one by the
repository's format. A challenge the format's `render_error` chose is never overwritten.

| Format | Challenge |
|---|---|
| `pub` | `Bearer realm="pub"` on 401 and on 403 (README 8.5: a missing or invalid token is 401; a valid token without rights is 403, which pub clients answer by keeping the stored token) |
| `docker`, `oci` | `Bearer realm="<origin>/v2/token",service="rustryx"` (plus `scope="repository:<repository>/<image>:pull"` on a read of `/v2/<repository>/<image>/...`; a write asks for `pull,push`, a delete for `pull,delete`) |
| everything else | `Basic realm="rustryx"`; maven and pip send credentials only after this 401 |

The pub rule is implemented in the harness, by repository format, because format-core has no
hook for a challenge: `FormatHandler::render_error` can set one, `ErrorInfo::anonymous` says
whether the caller was anonymous, and the harness fills in only what is missing. A pub handler
that overrides `hide_inaccessible_repository` to `false` and returns 403 for a hidden
repository gets the pub behaviour end to end.

## 5. OCI: `/v2/` and `/v2/token`

The harness owns these routes, as the server does (the API crate mounts them, not the format).

| Request | Answer |
|---|---|
| `GET /v2/` or `/v2` without a valid credential | 401 with the Bearer challenge above and `Docker-Distribution-API-Version: registry/2.0`. **Always**, also when anonymous pull is on: the client then fetches an anonymous token. |
| `GET /v2/` with a valid registry token, API token or Basic credential | 200 `{}`. `docker login` verifies the token it was just given with this request and fails on anything but 200. |
| `GET /v2/token?service=..&scope=..` with Basic credentials (password, or an API token in the password position) | `{"token", "access_token", "expires_in", "issued_at"}`, a short-lived `v2t_` bearer token |
| the same without credentials | an anonymous token (always issued; whether it is worth anything is `Rbac`'s call: the global switch and the repository flag) |
| the same with a wrong credential | 401 |
| `POST /v2/token` with `grant_type=password` (OAuth2 form) | `{"access_token", ...}`; any other grant is 400 `unsupported_grant_type` |
| `/v2/<repository>/<image>/...` | served by the repository `<repository>` |

A registry token is accepted wherever an `Authorization: Bearer` header is, on `/v2/` and on
`/repository/`. The scopes of an API token carry over to the registry token it was exchanged
for, so exchanging never widens a credential.

**Path mapping.** The first segment below `/v2/` is the Rustryx repository (a `RepoName` is
valid as a docker repository prefix, so `docker pull host:port/docker-hosted/team/app:1`
addresses repository `docker-hosted`, image `team/app`). The format sees the same
`head.path` whichever door the client used:

```
GET /v2/docker-hosted/team/app/manifests/1
GET /repository/docker-hosted/v2/team/app/manifests/1
        -> head.path == "v2/team/app/manifests/1", repository "docker-hosted"
```

## 6. Proxy tests

### 6.1 `UpstreamStub`: a real HTTP upstream

A wiremock server on loopback. Use it when the HTTP path matters (the guard, the resolver,
redirects, timeouts); it is slower than a fake client.

```rust
let upstream = UpstreamStub::start().await;
upstream.serve_file("/org/acme/a/1.0/a-1.0.jar", jar_bytes, "application/java-archive").await;
server.add_repository(NewTestRepo::proxy("central", RepoFormat::Maven2, &upstream.base_url())).await;
```

| Method | Effect |
|---|---|
| `url()`, `base_url()` | `http://127.0.0.1:PORT`, and with a trailing slash (a `remote_url`) |
| `serve_file(path, body, content_type)` | `GET` and `HEAD` with a matching `Content-Length` |
| `serve_file_with_headers(path, body, ct, &[(name, value)])` | plus `ETag`, `Last-Modified` |
| `not_found(path)`, `status(path, code)` | 404, or any status (500, 503, 401, 403) |
| `flaky(path, failures, status, body)` | fails `failures` times, then serves `body` (retry tests) |
| `slow(path, body, delay)` | waits before the response head |
| `truncated(path, body, declared_len)` | announces more bytes than it sends: the connection ends mid-body |
| `redirect(path, status, location)` | a redirect |
| `requests()`, `request_count()`, `count(method, path)` | what it received (`RecordedCall` has `header(name)`, lowercase names) |
| `reset()` | forgets scripts and the log |
| `server()` | the `wiremock::MockServer` |

### 6.2 `FakeUpstream`: no HTTP at all

`rustryx_testserver::FakeUpstream` and `Reply` are format-core's scripted client. Pass it with
`builder.upstream_client(fake.clone())`. Routes by URL path, counts calls, can delay, hang, break
mid-body or fail. It skips DNS and TCP, so a test of retry or breaker logic is fast and
deterministic.

### 6.3 The SSRF policy

The harness default (`loopback_friendly_policy()`) accepts any public host over https and,
for the names `127.0.0.1`, `localhost` and `::1` only, plain http and the loopback ranges.
Other private ranges, link-local and the cloud metadata address stay refused. **Production is
stricter**: `UpstreamUrlPolicy::default()` denies every host, and the composition root allows
named hosts, https only, with CIDRs for internal mirrors. A test of the guard itself passes
its own policy; `UpstreamUrlPolicy::default()` makes the loopback stub unreachable and the proxy
answers 502.

A proxy answers an upstream 401 or 403 as 502 (never forwarded), a 404 as 404, a 5xx after the
retries as 502 or 504.

## 7. Real clients

Real-client tests are slow and need tools that not every machine has, so they are gated.

* They run only with `RUSTRYX_IT=1`. Without it they print `SKIP: ...` and pass.
* Each test asks `require(tool).await` first, or uses the macro
  `require_tool!(Tool::Mvn);` as its first line, which `return`s from the async test when the
  tool cannot run. The SKIP line names the reason: tests off, tool not on `PATH`, Docker
  daemon not reachable, the `dart:stable` image not present, no Python venv, `twine` not
  installable (PyPI unreachable).

```
RUSTRYX_IT=1 cargo test -p rustryx-format-maven --locked -- --nocapture
```

### 7.1 This machine

| Tool | On this machine | Used as |
|---|---|---|
| `mvn`, `java` | Maven 3.10.0, Java 27 (Homebrew) | `Tool::Mvn` |
| `npm`, `node` | npm 11.20.0 | `Tool::Npm` |
| `pip` | pyenv Python 3.11.13 | `Tool::Pip`, from a venv ([7.3](#73-python-pip-and-twine)) |
| `twine` | **not installed** | `Tool::Twine`, installed into the venv from PyPI on first use |
| `skopeo` | 1.24.1 | `Tool::Skopeo` |
| `docker` | 29.8.2, OrbStack daemon; images `dart:stable` (Dart 3.13.6) present | `Tool::Docker` |
| `helm` | v4.3.0 | `Tool::Helm` |
| `dart`, `flutter` | **not installed** | `Tool::DartDocker`: the `dart:stable` image |
| `crane` | **not installed** | use `skopeo` |

The Maven plugins are cached in `~/.m2`, so `mvn deploy` resolves them offline; the first
`twine` run needs PyPI.

### 7.2 Helpers

* `TempProject`: a temporary directory removed on drop. `write(rel, contents)` creates parents
  and refuses an absolute path or `..`; `read(rel)`, `path()`, `join(rel)`.
  `isolated_env()` returns `HOME`, the XDG directories and the npm, pip and pub caches, all
  inside the project, as `Vec<(String, String)>`; `TempProject::env_pairs(&env)` turns it into
  the `&[(&str, &str)]` that `run` takes. Do not use it for `mvn`, which needs the user's
  `~/.m2` to resolve its plugins offline.
* `NewTestRepo`, `TestUser`: as above. Pass credentials in the client's own configuration, never
  on a command line you print.

### 7.3 Python: pip and twine

`Tool::Pip` and `Tool::Twine` run executables of a shared virtual environment, created on first
use with `python3 -m venv` in `$TMPDIR/rustryx-testserver-venv` (`RUSTRYX_IT_VENV` overrides
the path; 55 MB with `twine`). A directory used as a lock (`mkdir` is atomic) lets the first
test process create it while the others wait, so seven crates testing in parallel do not each
download `twine`. `twine` is installed from PyPI only when missing; when PyPI cannot be
reached the twine tests skip.

Do not install into the shared venv: two tests that publish different content under one name
would race. Use `pip install --target <dir>` (or `--dry-run`, `pip download -d <dir>`). The
runner sets `PIP_CONFIG_FILE=/dev/null`, `PIP_NO_INPUT=1` and
`PIP_DISABLE_PIP_VERSION_CHECK=1`.

### 7.4 The runner

```rust
pub async fn run(tool: Tool, args: &[&str], env: &[(&str, &str)], workdir: &Path) -> Output;
pub async fn run_with_timeout(tool, args, env, workdir, timeout) -> Output;
pub async fn run_program(program: &str, args, env, workdir, timeout) -> Output;  // curl, sh, ...
```

`Output { command, code: Option<i32>, stdout, stderr, timed_out }` with `success()`,
`combined()` and `assert_success()` (panics with the command and both streams).

* stdin is `/dev/null` (a prompt fails instead of hanging); stdout and stderr are captured.
* The default timeout is 300 seconds (`RUSTRYX_IT_TIMEOUT_SECS`); the client is killed on
  expiry, and `timed_out` is set.
* `CI=1` is set for every client.
* Anything that looks like a token the harness generated (`ryx_...`, `v2t_...`) is replaced by
  `[REDACTED-TOKEN]` in the captured output and in `command`.
* `Tool::Docker` runs with `DOCKER_CONFIG=<workdir>/.docker-config`, so `docker login` never
  touches the user's credentials (or their keychain helper), and with `DOCKER_HOST` taken from
  the current docker context, so the isolated configuration still finds the daemon.
* `Tool::Helm` runs with `HELM_REGISTRY_CONFIG`, `HELM_REPOSITORY_CONFIG`, `HELM_*_HOME` and
  the caches inside `<workdir>/.helm`.
* `Tool::DartDocker` runs `args` as a command inside the container:

  ```
  docker run --rm --name rustryx-it-<id> \
    --add-host host.docker.internal:host-gateway --add-host localhost:host-gateway \
    -v <workdir>:/work -w /work --user <uid>:<gid> \
    -e HOME=/work/.home -e PUB_CACHE=/work/.pub-cache \
    -e <KEY> ...  dart:stable <args...>
  ```

  Each `env` entry becomes `-e KEY` with the value in the docker client's environment, so a
  token is not in the argument list. `RUSTRYX_DART_IMAGE` replaces the image. The container is
  removed if the run times out. The image is never pulled automatically (it is over a
  gigabyte): run `docker pull dart:stable` once.

  **pub and plain http.** `dart pub token add http://host.docker.internal:PORT` is refused
  ("insecure repositories cannot use authentication"): pub sends a token only to https or to
  `localhost`. Inside the container `localhost` is the container, so the runner adds a second
  `/etc/hosts` line that maps `localhost` to the host as well (the resolver returns both
  addresses and the client falls through to the one that answers). **Use
  `server.localhost_repository_url("name")` for pub**, with `bind_all(true)`. Verified with
  the real `dart pub get`: the token arrives and a missing package is a 404, not a 401.

### 7.5 One complete example per tool

The mvn, skopeo, helm, docker and dart examples are tests of this crate (`tests/clients.rs`)
and pass. The npm, pip and twine examples show the client configuration the harness accepts;
they need a real format handler behind them, so they are templates for the format crates.

**mvn** (Maven 3.10; `deploy` of a pom-only project; credentials after the 401 challenge):

```rust
require_tool!(Tool::Mvn);
let server = TestServer::builder().register(Arc::new(MavenHandler::new())).start().await;
server.add_repository(NewTestRepo::hosted("maven-hosted", RepoFormat::Maven2)).await;
let alice = server.add_user("alice", &["nx-developer"]).await;
let project = TempProject::new();
project.write("pom.xml", format!(r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion><groupId>org.acme</groupId><artifactId>demo</artifactId>
  <version>1.0</version><packaging>pom</packaging>
  <distributionManagement><repository><id>rustryx</id><url>{}</url></repository></distributionManagement>
</project>"#, server.repository_url("maven-hosted"))).await;
project.write("settings.xml", format!(
    "<settings><servers><server><id>rustryx</id><username>alice</username><password>{}</password></server></servers></settings>",
    alice.token)).await;
run(Tool::Mvn, &["-B", "-ntp", "-s", "settings.xml", "deploy"], &[], project.path())
    .await.assert_success();
```

To resolve from the repository, add `<repositories>` with the same URL to a consumer `pom.xml`
and a `<server>` with the same `<id>` to `settings.xml`, then run
`mvn -B -ntp -s settings.xml dependency:get -Dartifact=org.acme:demo:1.0:pom`. Do not isolate
`HOME` for mvn: the plugins come from `~/.m2`.

**npm** (`.npmrc` in the project; the token is sent as `_authToken`, also over http):

```rust
require_tool!(Tool::Npm);
let reg = server.repository_url("npm-hosted");              // http://127.0.0.1:PORT/repository/npm-hosted
let host_path = reg.trim_start_matches("http:");             // //127.0.0.1:PORT/repository/npm-hosted
project.write(".npmrc", format!("registry={reg}/\n{host_path}/:_authToken={}\n", alice.token)).await;
project.write("package.json", r#"{"name":"demo-pkg","version":"1.0.0"}"#).await;
let env = project.isolated_env();
run(Tool::Npm, &["publish"], &TempProject::env_pairs(&env), project.path()).await.assert_success();
// install elsewhere: `npm install demo-pkg` in a second TempProject with the same .npmrc
```

**pip** (credentials in the index URL are sent as Basic; install into a target directory):

```rust
require_tool!(Tool::Pip);
let index = format!("http://alice:{}@127.0.0.1:{}/repository/pypi-group/simple/", alice.token, server.port());
run(Tool::Pip, &["install", "--no-deps", "--target", "site", "--index-url", &index,
                 "--trusted-host", "127.0.0.1", "demo-pkg==1.0"], &[], project.path())
    .await.assert_success();
```

**twine** (the user name is ignored when the password is a `ryx_` token, `__token__` is what
twine documentation uses):

```rust
require_tool!(Tool::Twine);
// dist/ built beforehand: write a tiny wheel or sdist with TempProject, or `pip wheel`.
run(Tool::Twine, &["upload", "--non-interactive", "--repository-url",
                   &format!("{}/", server.repository_url("pypi-hosted")),
                   "-u", "__token__", "-p", &alice.token, "dist/*"], &[], project.path())
    .await.assert_success();
```

**skopeo** (login follows the challenge to `/v2/token`; `--tls-verify=false` for plain http;
use an auth file inside the project):

```rust
require_tool!(Tool::Skopeo);
let registry = format!("127.0.0.1:{}", server.port());
let auth = project.join("auth.json").to_string_lossy().into_owned();
run(Tool::Skopeo, &["login", "--tls-verify=false", "--authfile", &auth, "-u", "alice", "-p", &alice.token, &registry],
    &[], project.path()).await.assert_success();
// push:  skopeo copy --dest-tls-verify=false --authfile <auth> docker-archive:img.tar docker://<registry>/docker-hosted/app:1
// pull:  skopeo copy --src-tls-verify=false  --authfile <auth> docker://<registry>/docker-hosted/app:1 dir:out
```

**docker** (the daemon runs the login, so it must reach `127.0.0.1:PORT` and trust it as an
insecure registry: true for OrbStack and a Linux daemon, not for a daemon in a VM that does not
share the host's loopback):

```rust
require_tool!(Tool::Docker);
let registry = format!("127.0.0.1:{}", server.port());
run(Tool::Docker, &["login", &registry, "-u", "alice", "-p", &alice.token], &[], project.path())
    .await.assert_success();                                 // "Login Succeeded"
// docker pull/push <registry>/docker-hosted/app:1
```

**helm** (OCI registry login over plain http; classic `helm repo add` takes
`http://127.0.0.1:PORT/repository/helm-hosted --username alice --password <token>`):

```rust
require_tool!(Tool::Helm);
let registry = format!("127.0.0.1:{}", server.port());
run(Tool::Helm, &["registry", "login", &registry, "--plain-http", "-u", "alice", "-p", &alice.token],
    &[], project.path()).await.assert_success();
// helm push chart.tgz oci://<registry>/oci-hosted --plain-http
```

**dart** (inside `dart:stable`; the server must be started with `bind_all(true)`):

```rust
require_tool!(Tool::DartDocker);
let server = TestServer::builder().bind_all(true).register(Arc::new(PubHandler::new())).start().await;
server.add_repository(NewTestRepo::hosted("pub-hosted", RepoFormat::Pub)).await;
let alice = server.add_user("alice", &["nx-developer"]).await;
let url = server.localhost_repository_url("pub-hosted");   // http://localhost:PORT/repository/pub-hosted
project.write("pubspec.yaml", format!("name: probe\nenvironment:\n  sdk: ^3.0.0\ndependencies:\n  demo:\n    hosted: {url}\n    version: ^1.0.0\n")).await;
run(Tool::DartDocker,
    &["sh", "-c", &format!("dart pub token add {url} --env-var PUB_TOKEN && dart pub get")],
    &[("PUB_TOKEN", alice.token.as_str())], project.path()).await.assert_success();
// publish: `dart pub publish --force` with PUB_HOSTED_URL=<url> in env
```

## 8. Tests of the harness itself

`cargo test -p rustryx-testserver --locked` runs them without `RUSTRYX_IT`.

| File | Covers |
|---|---|
| `src/*` unit tests | header parsing, token shape and expiry, scope of a registry challenge, the venv lock, `Command` composition per tool, redaction |
| `tests/http.rs` | hosted upload and download over HTTP with all credential forms; 401, 403 and 404; the anonymous gates (both switches, writes); group fan-out and index merge; proxy fill, hit, 404, 5xx, 403, flaky; the SSRF default; `FakeUpstream`; write policy and scoped tokens; content selectors and role changes; the pub challenges; routes, URLs, request ids, a raw path; fixture errors; shutdown; offline repository |
| `tests/oci.rs` | `/v2/` ping, token endpoint, OAuth2 grant, anonymous tokens, expiry, both doors, scope in the challenge |
| `tests/memory.rs` | 256 MiB streams through upload, download, proxy fill and cache hit with an RSS growth under 64 MiB (`RUSTRYX_MEMTEST_MIB` changes the size) |
| `tests/upstream.rs` | the stub, redirects, a truncated upstream body is neither served as complete nor cached |
| `tests/runner.rs` | the runner (output, exit code, env, stdin, timeout, spawn failure, redaction), `TempProject` |
| `tests/clients.rs` | gated: every available client runs; mvn deploy; skopeo, helm and docker login; the Dart container and pub's token |

Negative requirements are tests whose name starts with `must_not_`.

## 9. Limits and known gaps

* One blob store serves every repository; blob store groups are not modelled. The default
  `MemBlobStore` keeps bytes in memory: pass `SyntheticBlobStore` for very large bodies.
* The harness authenticator knows local users, API tokens and registry tokens. LDAP, OIDC,
  robot tokens and the login guard are `rustryx-auth`'s and are not wired here.
* HTTP/1.1 only, no TLS. A client that insists on https (npm for some configurations, docker
  against a non-loopback host) needs a loopback address or its insecure switch.
* The metadata store is the testkit double, looser than PostgreSQL where `docs/testkit.md`
  section 4 says so.
* The download counter batcher and the policy hook are not installed; use `customize_ports`.
* Docker: a daemon that does not share the host's loopback cannot log in to `127.0.0.1:PORT`.
  skopeo and helm run client side and do not care.
* The shared Python venv stays in `$TMPDIR` after the tests (55 MB); delete
  `rustryx-testserver-venv` there to reclaim it. Nothing else is left behind: containers are
  `--rm`, project directories are removed on drop, the `.lock` directory is removed on release.
