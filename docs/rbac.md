# Authorization (`rustryx-rbac`)

`rustryx-rbac` answers one question: may this principal perform this action on
this content. It never authenticates: who the caller is, and which roles the
caller holds for this request, is decided by `rustryx-auth` and arrives in the
`Principal`. Normative sources: README section 8.5, `01-architecture` section 1.5,
`03-security` section 2 (SEC-11 to SEC-14, SEC-18, SEC-19).

Contents: [decision model](#decision-model), [anonymous access](#anonymous-access),
[selector language](#content-selector-language), [token scopes](#api-token-scopes),
[group repositories](#group-repositories), [questions beyond content](#questions-beyond-content),
[caching and invalidation](#caching-and-invalidation), [metrics](#metrics), [wiring](#wiring),
[decision records](#decision-records), [gaps in core](#gaps-in-rustryx-core),
[tests](#tests).

## Decision model

`Rbac` implements `rustryx_core::ports::RbacChecker`. For `check(principal, action,
resource)` the rules apply in this order and the first that decides wins:

1. **Anonymous gates**, before any store access. A principal is anonymous when
   `auth_method == Anonymous`. An anonymous caller is refused when it asks to write or
   delete, when `resource.allow_anonymous` is false, or when the global switch is off (see
   [anonymous access](#anonymous-access)).
2. **Token scopes**: if the request carries an API token with scopes, an action
   outside the scopes is denied (see [token scopes](#api-token-scopes)).
3. **Administrator**: the principal holds the role that the store knows as
   `nx-admin` (matched by role id, see ADR-RBAC-4). Allowed.
4. **Privileges**: the effective privileges of the principal's roles
   (`RoleRepo::effective_privileges`, nesting expanded by the repository) are
   scanned. One privilege that lists the action and whose grant matches allows.
   There are no deny rules.
5. Otherwise **deny**.

A robot or scan token (`AuthMethod::RobotToken`, `ScanToken`) holds no role: the role
references it carries are ignored, exactly like those of an anonymous principal, so it gets
nothing from rules 3 and 4. The scope based rule for robots (design 3.e rule 4) and the
project roles (rule 6) belong to wave 2 stream B; see [projects](#projects). Until they
land a robot is refused everything, which is the safe side.

Grants:

| Grant | Matches when |
|---|---|
| `Admin` | always, except for anonymous callers (an anonymous principal never gets it) |
| `RepositoryView { format, repository }` | `repository` is `*` or equals the resource repository, and `format` is `None` or equals the resource format |
| `RepositoryContentSelector { repository, selector_id }` | the repository matches as above and the selector evaluates to true for the resource |
| `Application { area }` | never, for content. It administers an area and is read only by `check_app` (see [application areas](#application-areas)) |

`PrivilegeGrant` is `#[non_exhaustive]`: a grant this build does not know matches nothing,
for content and for visibility alike. An unknown value is never an allow.

Repository names are matched exactly and case sensitively. `*` is the only
wildcard and only as the whole value.

Anonymous principals are checked as the built-in role `nx-anonymous`, looked up
by name in the store. The roles an anonymous `Principal` carries are ignored, so
a forged `nx-admin` role reference on an anonymous principal changes nothing. An
authenticated user without any role is **not** treated as anonymous (README 8.5).
If the store has no `nx-anonymous` role, anonymous callers get nothing.

`can_see_repository` (used to answer 404 instead of 403) is true when some
privilege that applies to the repository allows any action, within the token
scopes. For anonymous callers only `browse` and `read` count, and nothing is visible while
the global anonymous switch is off. A selector privilege counts only while its selector
exists and compiles; which paths it matches is not evaluated. Administrators see everything
their scopes allow. The signature carries a name and a format, so the repository's own
`allow_anonymous` flag is not visible here (see [gaps in core](#gaps-in-rustryx-core), gap 1).

### Failure semantics

| Situation | Result |
|---|---|
| Selector does not compile, was deleted, or cannot be evaluated | the privilege contributes nothing; other privileges still apply |
| Path is not canonical (`.` or `..` segment, `//`, control character) | selector privileges do not match |
| Store or selector lookup fails | `Err(AuthError::Backend { .. })`, never `Ok(Allow)`; callers must treat `Err` as a refusal |
| Cached answer older than its TTL | not used; a failed reload is an error, the old answer is not served |

Why `Err` and not `Ok(Deny)` for a store failure: the HTTP layer can then answer
503 and log the cause, instead of an unexplained wave of 403. Both are refusals;
what the contract forbids is any path from a store failure to `Allow`.

## Anonymous access

Anonymous access needs two things, and `check` enforces both (CONTRIBUTING rule 4, core gap
13). The old arrangement left them to the HTTP layer, where a forgotten call meant an open
repository; now a call site that forgets still gets a refusal.

| Gate | Where it comes from | Cannot be forgotten because |
|---|---|---|
| repository flag | `ResourceRef::allow_anonymous`, copied from `repositories.allow_anonymous` by `ResourceRef::for_repository` | the constructor takes the repository, and the type is `#[non_exhaustive]` |
| global switch | `RbacConfig::anonymous_enabled` (`auth.anonymous_enabled`), default **false** | the authorizer owns it |

Both are evaluated per request from the request alone, ahead of every cache and store read:
an anonymous flood against a closed repository costs no store access, and a store outage
turns into a definite refusal, not an error. Nothing about them is cached.

The switch is a shared handle, `Rbac::anonymous_switch()` returning an `AnonymousSwitch`.
Clones of it (and of the `Rbac`) control one flag, so the settings layer can keep a clone and
flip it at run time; turning it off refuses the very next anonymous request on this node,
whatever is cached. `RbacConfig::anonymous_enabled` is only the initial position. The default
is closed: a composition root that forgets the setting gets an authorizer that refuses
anonymous callers.

`filter_visible_members` applies the flag of each member, not the flag of the group (see
[group repositories](#group-repositories)).

## Content selector language

A selector is a boolean expression. The engine is `RestrictedEngine`, a parser
and evaluator for a subset of CEL plus the `=~` operator (ADR-RBAC-1).

### Variables

| Name | Type | Value |
|---|---|---|
| `format` | string | canonical lowercase label: `maven2`, `npm`, `pypi`, `docker`, `oci`, `helm`, `raw`, `pub` |
| `path` | string | normalized path **with a leading `/`** (one is added when the format's path has none) |
| `repository` | string | repository name |
| `coordinate.group` | string | namespace (maven groupId, npm scope). Alias: `coordinate.groupId` |
| `coordinate.name` | string | package name. Alias: `coordinate.artifactId` |
| `coordinate.version` | string | version |
| `coordinate.extension` | string | file extension (maven: `jar`, `pom`, `war`) |
| `coordinate.classifier` | string | classifier (maven: `sources`, `javadoc`, `tests`) |

The aliases exist so that maven style selectors (`coordinate.groupId`,
`coordinate.artifactId`) work unchanged. Any other `coordinate.x` is a compile
error. `coordinate` alone is not a value.

`extension` and `classifier` are `Option` fields of `Coordinate` (core gap 12). `None` is
*unknown*, not empty (see the next section), so a format that can tell that a component has
no classifier must pass `Some("")`, and a selector then matches it with
`coordinate.classifier == ""`. A format with no such notion (npm, pypi) leaves both `None`,
and a selector that names them matches nothing there.

### Operators, methods, literals

| Construct | Meaning |
|---|---|
| `a == b`, `a != b` | string equality, exact and case sensitive |
| `a =~ "re"` | the regex must match the **whole** string (anchored implicitly) |
| `a.matches("re")` | the regex may match **any part** of the string (CEL semantics) |
| `a.startsWith(b)`, `a.endsWith(b)`, `a.contains(b)` | string tests; `b` may be any string expression |
| `a in ["x", "y"]` | membership in a list of string literals (1 to 64 items) |
| `x && y`, `x \|\| y`, `!x`, `( x )` | boolean logic, usual precedence |
| `true`, `false` | boolean literals |
| `"..."`, `'...'` | string literals with escapes `\\ \" \' \n \r \t` |
| `r"..."`, `r'...'` | raw strings: no escapes, convenient for regexes |

Everything else is a compile error (arithmetic, `?:`, macros such as `exists`,
integers, `has()`, other methods, dynamic regex patterns). The pattern of `=~` and
`matches` must be a string literal so that it is compiled and size checked once,
when the selector is saved.

The regex dialect is Rust's `regex` crate: no backreferences and no lookaround.
Matching is linear in the input, never exponential. The two anchoring semantics
differ on purpose: `=~` follows the JEXL convention content selectors came from
(whole string), `matches` follows the CEL specification (any substring). The
exact behaviour of `=~` in the original product was not verified against a
running instance in this work; if migration shows a difference, the single place
to change is `src/selector/parser/pattern.rs`.

### Unknown values (three valued logic)

Some inputs may not exist: the coordinate of a path whose format cannot derive
one, or a coordinate field that is `None` (a `version`, an `extension`, a `classifier`). Such a value is *unknown*, and
unknown propagates like CEL's error value: `true || unknown` is true,
`false && unknown` is false, anything else that touches an unknown is unknown, and
an unknown result is an evaluation error, which means "no match".

The invariant: a selector matches only when it is true for every value the
missing data could have had. So `!(coordinate.groupId == "evil")` does **not** grant
access to a path whose group is not known, and `coordinate.classifier != "sources"` does
not grant a component whose classifier nobody supplied
(`must_not_match_a_negated_comparison_on_an_unknown_classifier`).

### Limits (all apply when the selector is saved or loaded)

| Limit | Default | On violation |
|---|---|---|
| expression length | 1024 bytes (SEC-18) | compile error `TooLong` |
| nesting of parentheses, `!`, call arguments | 32 | compile error `TooDeep` (caps parser recursion, so no stack overflow) |
| `in` list items | 64 | compile error |
| regex source length | 512 bytes | compile error |
| compiled regex size | 1 MiB (`regex` `size_limit` and `dfa_size_limit`), nesting 32 | compile error |
| input string length (`path`, `repository`, each coordinate field) | 8192 bytes | evaluation error |
| nodes visited per evaluation | 4096 | evaluation error |

There is no wall clock timeout. The tree has no loops and no recursion over data,
every node is visited at most once, and regex matching is linear in an input that
is capped, so the work of one evaluation is bounded by construction. The node
budget is a backstop for a future grammar change; a 1 KiB expression visits at
most a few hundred nodes.

Limits are configurable through `RestrictedEngine::new(SelectorLimits { .. })` and
passed with `Rbac::with_engine`.

### The path a selector sees

Format crates normalize the path once (SEC-19) and pass the same string to
authorization and storage. As a second line of defense, a path that is not
canonical never matches a selector privilege: a `.` or `..` segment, an empty
segment other than one trailing slash, or any control character. So
`path.startsWith("/public/")` cannot be satisfied by `/public/../secret`. Rbac does
not decode percent escapes: it must see the exact string storage will use.
Repository wide privileges do not look at the path and are unaffected.

## API token scopes

A token can only narrow what its owner may do, never widen it: the effective right
of a token request is the intersection of the owner's privileges and the scopes.
`Principal.token_scopes` carries the scope strings.

```text
scope  = "*" | "repo:" target ":" action
target = repository name | "*"
action = "browse" | "read" | "write" | "delete" | "*"
```

- `*` lifts the restriction (the owner's rights).
- A list is a union: the request must be covered by at least one scope.
- `None` and an empty list mean no restriction (`ApiToken`: "empty means owner's rights").
- A non-empty list whose entries are all malformed restricts everything. A
  malformed entry is ignored, never guessed, so a typo narrows a token instead of
  widening it. Token creation should call `rustryx_rbac::parse_scope` to reject
  bad scopes up front.
- Matching is exact and case sensitive; `repo:*:*` is not the same as `*` for the
  purpose of `is_admin`.
- Scopes narrow administrators too. `is_admin` is false for a restricted token, and so are
  `check_app` and `check_project` (see [questions beyond content](#questions-beyond-content)).

## Group repositories

Authorization is per member (SEC-13). The group pipeline must:

1. `check` the principal against the **group** repository itself, then
2. serve data only from the members returned by `Rbac::filter_visible_members(principal,
   action, members, path, coordinate)`, where `members` is a slice of `Repository` (or
   `Arc<Repository>`, or `&Repository`: anything that borrows as one).

The helper decides every member with the same rules as `check` (the anonymous gates,
privileges, selectors with the requested path and coordinate, token scopes), loads the
privileges once, keeps the group's order and silently leaves refused members out,
which does not reveal that they exist. A store failure is an error and returns no
members, also when it happens halfway through the list.

The members are repositories and not names because the decision needs what a `ResourceRef`
carries for each one: its own format (so a format restriction of a grant is judged against the
member, not against the group) and its own `allow_anonymous` flag. An anonymous caller never
receives a member that did not opt in, even from a group that did; with names only that could
not be told. Revision 1 of this function took `format` and `&[RepoName]`; nothing in the
workspace called it.

## Questions beyond content

`RbacChecker` gained five provided methods in Core revision 2. Each has a fail closed default
in core, and `Rbac` overrides all of them. They share three rules: an anonymous caller, a
robot or scan token, and a restricted token (a non empty scope list without a lone `*`) hold
nothing here, because the scope grammar describes content and reading it as a statement
about administration would widen it; a store failure is `Err`, never an allow; a role is
recognised by id, never by name.

### Administrator

`is_admin(principal)`: authenticated, holds the role that the store knows as `nx-admin`
(matched by id, ADR-RBAC-4), token unrestricted. It replaces the inherent method of revision 1
and the removed `Principal::is_admin()` (core gap 14 and 15).

### Application areas

`check_app(principal, area, action)` answers for the administration areas
(`app.<area>.read` and `.write`). Allowed for `nx-admin`, and for a role privilege
`Application { area }` that lists the action (`Read` is read, `Write` is create and update,
`Delete` is delete). Two things do **not** pass: an `Admin` privilege held by another role
(it means every action on content; administration is gated by the role id of `nx-admin` or an
explicit area grant, so there is no second route to administrator rights), and an
`Application` privilege used for content (`check` never reads it). The api crate evaluates
`App(..)` as `Admin` until the `ApplicationPrivileges` feature level is open (wave 2 design
2.8); `check_app` is what it calls afterwards.

### Delegation to robots

`holds_whole_repository(principal, repository, action)` is the test for handing an action to a
robot token (a robot cannot exceed its creator): `nx-admin`, an `Admin` privilege, or a
`RepositoryView` privilege that covers the repository (name and format) and lists the action,
all within the token scopes. A **content selector never counts**: a robot would carry the
action without the selector, which is the whole repository. Anonymous callers and robots hold
nothing to delegate.

### Projects

`visible_projects(principal)` returns the filter that `ProjectRepo::list_visible` takes:

| Principal | Filter |
|---|---|
| `nx-admin`, unrestricted token | `All` |
| user, unrestricted token | `Member { user_id, provider, groups }`: `provider` is `Oidc` or `Ldap` by `Principal::source` and `groups` are `external_groups` unmodified (exact matching is the SQL's job); a local account has no provider, so it passes no groups either |
| restricted token, robot, scan token | `PublicOnly` (the smallest filter there is) |
| anonymous | `PublicOnly` while the anonymous switch is on, `Err(AuthError::AccessDenied("anonymous access is disabled"))` while it is off |

`check_project(principal, project, permission)` passes only for `nx-admin` with an unrestricted
token today; everyone else is refused. The membership rules (project roles through
`ProjectRole::template()`, the public project rule, the robot `scan` permission) are wave 2
stream B (design 3.e) and need the `ProjectAccessRepo` port wired into `Rbac`, which the
composition root does not have yet. The extension points are in place:

- `ResourceRef` already carries the project, and `filter_visible_members` builds its per
  member decision from the repository, so rule 6 of design 3.e slots into the one decision
  function;
- `InvalidationEvent::Projects` is applied (and counted) but drops nothing, because nothing
  cached here depends on projects; the `direct` and `groups` caches of design 3.e drop on it
  when they arrive;
- the version counter in the cache keys does not move for `Projects` or `Config`, so adding
  the two new caches changes no existing key.

## Caching and invalidation

| Cache | Key | Value | TTL | Capacity |
|---|---|---|---|---|
| privileges | exact sorted role ids, user id (or none), version | `Arc<[Privilege]>` | 60 s | 10 000 |
| built-in role ids | built-in name, version | role id or none | 60 s | 16 |
| selectors | selector id, version | compiled program or "does not compile" or "gone" | 60 s | 1 000 |
| programs | selector id, `updated_at` | compiled program (source kept to double check) | 300 s | 1 000 |

- **TTL never exceeds 5 minutes** (`MAX_DECISION_TTL`), whatever is configured. Two
  clocks enforce it: `moka`'s own real time TTL, and a `tokio::time::Instant` stamp on
  each entry checked on every read (which is what lets tests prove the TTL with paused
  time). Neither is the wall clock, so a time step cannot extend an entry.
- **A failed load is never cached** and the previous value is not served instead.
  Concurrent first requests for one key share one load.
- **Exact keys**: the privilege cache is keyed by the role ids themselves, never a hash of
  them, because a collision would hand one user another user's privileges. The role
  set comes from the principal of *this request*, so removing a group takes effect
  on the next request without waiting for the cache.
- **Version in every key**: a counter bumped by each invalidation. A load reads it before
  querying the store, so a load that was already running when the message arrived
  stores its result under the old version, where nothing will look. Without this a slow
  load could re-insert stale data right after `invalidate_all`.

`Rbac::spawn_invalidation_listener(bus, cancel)` subscribes **before** it returns (no
message published afterwards is lost) and spawns a task tied to the token. It
returns an `InvalidationListener`; keep it and `join().await` it during shutdown.

| Event | Effect |
|---|---|
| `Rbac` | bump version, drop privileges, built-in role ids and selectors (programs stay: they are pure) |
| `User(id)` | drop the privilege entries of that user |
| `Reset` | everything, including programs |
| `Projects` | nothing today: no cache here depends on project data. The `direct` and `groups` caches of wave 2 stream B drop on it |
| `Config(revision)` | nothing: no decision of this crate reads a setting (the anonymous switch has its own handle) |
| a variant core adds later | everything, like `Reset` (not knowing what it changed); counted as `unknown` |
| stream ends | `Reset`, log a warning, task ends; from then on the TTL is the only limit |

Every message is counted in `rbac_invalidations_total` (see [metrics](#metrics)).

## Metrics

Emitted through the `metrics` facade; the exporter is `rustryx-observability`. The names are
unprefixed like the rest of the catalogue in [slo.md](slo.md) and are exported as constants of
`rustryx_rbac::metrics`. Every label is a closed set: no principal, role, repository, path,
selector or group name ever becomes a label value (SRE-09; pinned by
`must_not_put_unbounded_values_in_labels`).

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `rbac_decisions_total` | counter | `kind`, `result` | one per question answered. `kind`: `content` (`check`, and every member of `filter_visible_members`), `app` (`check_app`), `project` (`check_project`), `admin` (`is_admin`). `result`: `allow`, `deny`, `error` (the store failed and the caller had to refuse, a 503 class event: tell it from a refusal) |
| `rbac_cache_events_total` | counter | `cache`, `event` | `cache`: `privileges`, `builtin_roles`, `selectors`, `programs`. `event`: `hit` (served without this call touching the store, including a call that waited for another call's load), `miss` (this call loaded from the store), `error` (the load failed, nothing was cached). Store reads are `miss + error` |
| `rbac_invalidations_total` | counter | `event` | invalidation messages applied on this node. `event`: `rbac`, `user`, `reset`, `projects`, `config`, `unknown`, `stream_end` (the bus closed, caches fell back to their TTL) |

Useful expressions: refusal share `sum(rate(rbac_decisions_total{result="deny"}[5m])) /
sum(rate(rbac_decisions_total[5m]))`; store trouble `rate(rbac_decisions_total{result="error"}[5m]) > 0`;
privilege cache hit ratio `sum(rate(rbac_cache_events_total{cache="privileges",event="hit"}[5m])) /
sum(rate(rbac_cache_events_total{cache="privileges"}[5m]))`; a bus that is gone
`increase(rbac_invalidations_total{event="stream_end"}[10m]) > 0`.

None of the three is in `rustryx-observability`'s `names.rs` yet, so none is in the
"Exported by the code today" table of slo.md; `deploy/monitoring/check_assets.py` reads that
table, so move the rows there when the observability owner lists the names.

## Wiring

```rust
use std::sync::Arc;
use rustryx_rbac::{Rbac, RbacConfig};

// db implements RoleRepo and SelectorRepo (one value, two trait objects).
// The anonymous switch comes from `auth.anonymous_enabled`; the default is closed.
let config = RbacConfig { anonymous_enabled: settings.auth.anonymous_enabled, ..RbacConfig::default() };
let rbac = Rbac::new(Arc::new(db.clone()), Arc::new(db.clone()), &config);
// Optional: let the settings layer flip the switch at run time.
let anonymous_switch = rbac.anonymous_switch();
// Keep the handle, join it on shutdown.
let listener = rbac.spawn_invalidation_listener(bus.as_ref(), shutdown.child_token());
let checker: Arc<dyn RbacChecker> = Arc::new(rbac.clone());
```

`Rbac` is cheap to clone (clones share the caches). `Rbac::with_engine` takes a custom
`SelectorEngine`. Extra methods beyond `RbacChecker`:

| Method | Use |
|---|---|
| `validate_selector(expression)` | compile when a selector is created or edited; return the error as a 400 (SEC-18) |
| `filter_visible_members(principal, action, members, path, coordinate)` | group pipeline, see above |
| `anonymous_switch()` | the shared handle of the global anonymous switch |
| `invalidate(&event)` | apply an event without the bus |

`is_admin`, `check_app`, `check_project`, `holds_whole_repository` and `visible_projects` are
methods of `RbacChecker` itself, so the api crate reaches them through `Arc<dyn RbacChecker>`.

What the caller still has to do, because rbac cannot know it:

- **The anonymous gates are no longer the caller's job.** `check` refuses an anonymous caller
  unless `resource.allow_anonymous` and the switch are both on, so the api crate's own
  `anonymous_enabled` test in `AppState::authorize` is now a second, redundant line of
  defense (it can stay, or be dropped, without changing what is allowed). What remains is the
  configuration: pass `auth.anonymous_enabled` into `RbacConfig`, and keep the two values in
  step if the settings layer flips the switch (use `anonymous_switch()`).
- **404 versus 403.** On a refusal call `can_see_repository`; false means 404. For an anonymous
  caller the answer ignores the repository's own flag (the signature cannot carry it), so the
  caller that holds the `Repository` should answer 404 itself when it is closed to anonymous
  callers.
- **Errors.** Treat `Err` from `check` as a refusal (503 with logging is the natural map).
- **Pub routes** (README 8.5): refusals, hidden repositories, selector denials and
  fail closed all answer 403 with the pub `WWW-Authenticate`. That mapping is the
  api crate's job.

## Decision records

### ADR-RBAC-1: restricted selector engine instead of the `cel` crate

**Context.** ADR 10 of `01-architecture` chose `cel-interpreter` (now the `cel` crate)
for content selectors and named the risk: incomplete coverage of the language. The
mitigation was to hide the engine behind a trait and to run a corpus of realistic
expressions; if the corpus does not pass, fall back to a restricted parser. The task
fixed the decision rule: use `cel` if the corpus passes.

**Evidence.** The corpus is `tests/fixtures/selector_corpus.json`: 24 realistic valid
expressions with 62 sample inputs (maven prefixes, regexes, coordinate group or name,
`format in [..]`, negation, nesting, missing coordinate) and 24 invalid expressions
that must be rejected. Each entry records the verdict of `cel` 0.15.0 in the field
`cel_0_15`. It was measured with a scratch project (variables `format`, `path`,
`repository` as strings and `coordinate` as a map with the maven aliases added),
macOS, rustc 1.99, `cel` 0.15.0 with default features.

| Check | Result with `cel` 0.15.0 |
|---|---|
| valid expressions that compile and give the expected answers | 20 of 24 |
| `path =~ "..."` (4 expressions) | **do not compile**: `=~` is not CEL, the parser reports a token recognition error. No extension point for operators |
| invalid expressions rejected when saved | 9 of 24, and two of those only because `=~` does not exist; the other 15 are accepted at compile time and fail on evaluation or give a wrong type (unknown identifier or field, non boolean result, `format == 1`, dynamic or invalid `matches` pattern, anything outside the subset) |
| regex handling | the pattern is compiled on **every evaluation** with default limits (10 MiB): 20 ms for `(a{100}){100}`, 75 ms for 125 000 repetitions, 0.56 s before failing on `(\w{50}){20}` (debug build; 47 ms in release) |
| evaluation limits | none. 3 nested comprehensions over 30 elements (about 300 bytes): 140 ms; 6 nested over 12 elements: 16 s (debug build) |
| parser stack safety | 200 nested parentheses overflow a 2 MiB thread stack in a debug build and abort the process; a release build stops at its own recursion limit of 96 and returns an error |

**Decision.** The corpus does not pass (4 of 24 cannot even be parsed), and the other
findings are the safety properties SEC-18 asks for that `cel` does not provide
(save time validation, regex cost, step limit, bounded recursion). So `rustryx-rbac`
ships `RestrictedEngine`, a hand written parser and evaluator for the subset listed
above. It keeps the `SelectorEngine` trait of ADR 10, so another engine can be
plugged in with `Rbac::with_engine`.

**Alternatives rejected.**
- Rewrite `=~` into `matches` textually before handing the expression to `cel`:
  fragile (string literals, precedence), and it fixes none of the safety findings.
- Wrap `cel` with limits: its evaluator offers no step budget or deadline hook, and
  the regex is compiled inside the call.
- Fork `cel`: a maintenance burden for a language subset that fits in about a thousand lines with comments.

**Consequences.**
- Expressions outside the subset are rejected when saved, with a byte offset, instead
  of failing on every request later.
- Behaviour is fully specified here and pinned by the corpus, a differential property
  test against an independent reference model (2000 cases, including unknown values),
  and parser fuzz style property tests.
- Revisit if `cel` gains an execution budget, a parse recursion limit that does not
  depend on the build profile, and a way to add `=~` (or the corpus is rewritten
  without it). The corpus and the trait are the acceptance test for a swap.

**Reproducing the measurement.** Create a project with `cel = "0.15"` and
`serde_json = "1"`, read the corpus, then for each expression call
`cel::Program::compile(expr)` and, for each input,
`program.execute(&ctx)` with a `cel::Context::default()` to which
`add_variable_from_value("path", ..)` etc. are added (the coordinate as a
`HashMap<String, cel::Value>` with `group`, `name`, `version`, `groupId`,
`artifactId`). Compile errors of the first four expressions appear immediately.

### ADR-RBAC-2: refusals are `Err` for infrastructure, `Deny` for data

A store or selector lookup failure is `Err(AuthError::Backend)`; a selector that is
broken, missing, non canonical input or a missing coordinate is a quiet no match.
Rationale: operators need to see an outage as 503 and in logs, while a broken
selector is a data problem of one privilege that must not lock out unrelated grants
(`must_not_let_a_selector_error_block_an_unrelated_grant`). Both are non allows.

### ADR-RBAC-3: cache design

Exact keys, version in the key, no stale fallback, two monotonic clocks, TTL capped at
five minutes, single flight loading. See [caching](#caching-and-invalidation) for the
mechanism and the reasons. Programs are cached by `(selector id, updated_at)` as the
architecture asks, and the source text is kept in the entry so that a store that
changes an expression without moving `updated_at` cannot leave a stale program in use.

### ADR-RBAC-4: administrator identity is a role id, not a role name

The phase 1 `Principal::is_admin()` compared the role **name** with `nx-admin` (Core
revision 2 removed it; `Principal::has_role` remains, documented as name based and for display
and tests only). That is sufficient
only if the names in the principal are trustworthy, and the trap behind ADR 7 is exactly
a group that is mapped to a role reference by name. Rbac therefore asks the store for
the id of the role named `nx-admin` and matches role **ids**: a reference that claims
the name with another id is not an administrator and gets only what its id grants;
the real role is an administrator whatever name it is carried under.
(`must_not_grant_admin_from_role_name_lookalike`,
`must_not_grant_admin_to_a_role_ref_that_only_claims_the_name`.) The same rule covers the new
questions: `check_app`, `check_project`, `holds_whole_repository` and `visible_projects` match
the role by id, and a robot or scan token never holds the role whatever references it carries
(`must_not_let_a_robot_or_scan_token_be_an_administrator`).

## Gaps in `rustryx-core`

Not edited here; listed for the owner of core. The four gaps of revision 1 are closed by
Core revision 2 (docs/core-gaps.md 12 to 15) and are adopted above. Found while adopting it:

1. **`RbacChecker::can_see_repository` takes a name and a format**, so it cannot apply the
   repository's `allow_anonymous` flag, and a decision that `check` now makes structural is
   advisory here. Taking a `&ResourceRef` or a `&Repository` would close it; until then the
   caller that holds the `Repository` answers 404 for an anonymous caller on a closed repository.
2. **`InvalidationEvent::User` still carries a bare `Uuid`**, converted with
   `UserId::from_uuid` (the old gap 4, decided cosmetic in design 1.2).
3. **No `ProjectAccessRepo` or `ProjectRepo` reaches `Rbac`**, because wave 2 stream B has not
   wired them. `check_project` therefore answers only for `nx-admin`, and it cannot tell a
   public project from a private one (its signature carries a `ProjectId`, so the visibility
   comes from a repository lookup the stream B work adds).
4. **`Coordinate` fields are `Option`**, so "this component has no classifier" and "nobody
   derived the classifier" look the same. rbac reads `None` as unknown (fail closed) and asks
   the format to pass `Some("")` for a known absence; the maven format crate must do so.

## Tests

`cargo test -p rustryx-rbac` (unit and integration, plus one doctest).

| File | Covers |
|---|---|
| `tests/selector_corpus.rs` + `fixtures/selector_corpus.json` | the corpus, and that the ADR numbers match the fixture |
| `tests/selector_limits.rs` | length, depth, regex cost, input size, step budget, unknown values, anchoring, errors |
| `tests/selector_props.rs` | never panics on arbitrary text, token soup, regexes; differential test against a reference model; path canonicalization |
| `tests/rbac_decisions.rs` | roles, nesting, grants, formats, selectors |
| `tests/rbac_access.rs` | anonymous, token scopes, visibility, store errors |
| `tests/rbac_anonymous.rs` | the two anonymous gates, the switch handle, no store access behind a closed gate |
| `tests/rbac_management.rs` | `is_admin`, `check_app`, `check_project`, robots and scan tokens |
| `tests/rbac_delegation.rs` | `holds_whole_repository` and `visible_projects` |
| `tests/rbac_metrics.rs` | `rbac_decisions_total`, `rbac_cache_events_total`, `rbac_invalidations_total`, label hygiene |
| `tests/selector_coordinate.rs` | `coordinate.extension` and `coordinate.classifier`, unknown semantics, maven style privileges |
| `tests/rbac_group.rs` | per member group filtering, per member format and anonymous flag |
| `tests/rbac_cache.rs` | caching, TTL (paused time), bus invalidation, in-flight load race, listener lifecycle |
| `tests/rbac_props.rs` | anonymous never writes and needs both gates, scopes never extend, privileges only allow, union of roles |
| `tests/rbac_must_not.rs` | every negative requirement, tests named `must_not_*` |

The tests run against the testkit's `MemStore` and `MemInvalidationBus`, wrapped to count
store reads and to fail selector reads.
