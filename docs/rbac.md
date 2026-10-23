# Authorization (`rustryx-rbac`)

`rustryx-rbac` answers one question: may this principal perform this action on
this content. It never authenticates: who the caller is, and which roles the
caller holds for this request, is decided by `rustryx-auth` and arrives in the
`Principal`. Normative sources: README section 8.5, `01-architecture` section 1.5,
`03-security` section 2 (SEC-11 to SEC-14, SEC-18, SEC-19).

Contents: [decision model](#decision-model), [selector language](#content-selector-language),
[token scopes](#api-token-scopes), [group repositories](#group-repositories),
[caching and invalidation](#caching-and-invalidation), [wiring](#wiring),
[decision records](#decision-records), [gaps in core](#gaps-in-rustryx-core),
[tests](#tests).

## Decision model

`Rbac` implements `rustryx_core::ports::RbacChecker`. For `check(principal, action,
resource)` the rules apply in this order and the first that decides wins:

1. **Anonymous write and delete are denied**, before any store access. A
   principal is anonymous when `auth_method == Anonymous`.
2. **Token scopes**: if the request carries an API token with scopes, an action
   outside the scopes is denied (see [token scopes](#api-token-scopes)).
3. **Administrator**: the principal holds the role that the store knows as
   `nx-admin` (matched by role id, see ADR-RBAC-4). Allowed.
4. **Privileges**: the effective privileges of the principal's roles
   (`RoleRepo::effective_privileges`, nesting expanded by the repository) are
   scanned. One privilege that lists the action and whose grant matches allows.
   There are no deny rules.
5. Otherwise **deny**.

Grants:

| Grant | Matches when |
|---|---|
| `Admin` | always, except for anonymous callers (an anonymous principal never gets it) |
| `RepositoryView { format, repository }` | `repository` is `*` or equals the resource repository, and `format` is `None` or equals the resource format |
| `RepositoryContentSelector { repository, selector_id }` | the repository matches as above and the selector evaluates to true for the resource |

Repository names are matched exactly and case sensitively. `*` is the only
wildcard and only as the whole value.

Anonymous principals are checked as the built-in role `nx-anonymous`, looked up
by name in the store. The roles an anonymous `Principal` carries are ignored, so
a forged `nx-admin` role reference on an anonymous principal changes nothing. An
authenticated user without any role is **not** treated as anonymous (README 8.5).
If the store has no `nx-anonymous` role, anonymous callers get nothing.

`can_see_repository` (used to answer 404 instead of 403) is true when some
privilege that applies to the repository allows any action, within the token
scopes. For anonymous callers only `browse` and `read` count. A selector
privilege counts only while its selector exists and compiles; which paths it
matches is not evaluated. Administrators see everything their scopes allow.

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

The aliases exist so that maven style selectors (`coordinate.groupId`,
`coordinate.artifactId`) work unchanged. Any other `coordinate.x` is a compile
error. `coordinate` alone is not a value.

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
one, or a coordinate whose `version` is empty. Such a value is *unknown*, and
unknown propagates like CEL's error value: `true || unknown` is true,
`false && unknown` is false, anything else that touches an unknown is unknown, and
an unknown result is an evaluation error, which means "no match".

The invariant: a selector matches only when it is true for every value the
missing data could have had. So `!(coordinate.groupId == "evil")` does **not** grant
access to a path whose group is not known.

### Limits (all apply when the selector is saved or loaded)

| Limit | Default | On violation |
|---|---|---|
| expression length | 1024 bytes (SEC-18) | compile error `TooLong` |
| nesting of parentheses, `!`, call arguments | 32 | compile error `TooDeep` (caps parser recursion, so no stack overflow) |
| `in` list items | 64 | compile error |
| regex source length | 512 bytes | compile error |
| compiled regex size | 1 MiB (`regex` `size_limit` and `dfa_size_limit`), nesting 32 | compile error |
| input string length (`path`, `repository`, coordinate parts) | 8192 bytes | evaluation error |
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
  purpose of `Rbac::is_admin`.
- Scopes narrow administrators too. `Rbac::is_admin` is false for a restricted token.

## Group repositories

Authorization is per member (SEC-13). The group pipeline must:

1. `check` the principal against the **group** repository itself, then
2. serve data only from the members returned by `Rbac::filter_visible_members(principal,
   action, format, members, path, coordinate)`.

The helper decides every member with the same rules as `check` (privileges,
selectors with the requested path and coordinate, token scopes), loads the
privileges once, keeps the group's order and silently leaves refused members out,
which does not reveal that they exist. A store failure is an error and returns no
members. Group members share the format of the group, hence the single `format`
argument.

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
| stream ends | `Reset`, log a warning, task ends; from then on the TTL is the only limit |

## Wiring

```rust
use std::sync::Arc;
use rustryx_rbac::{Rbac, RbacConfig};

// db implements RoleRepo and SelectorRepo (one value, two trait objects).
let rbac = Rbac::new(Arc::new(db.clone()), Arc::new(db.clone()), &RbacConfig::default());
// Keep the handle, join it on shutdown.
let listener = rbac.spawn_invalidation_listener(bus.as_ref(), shutdown.child_token());
let checker: Arc<dyn RbacChecker> = Arc::new(rbac.clone());
```

`Rbac` is cheap to clone (clones share the caches). `Rbac::with_engine` takes a custom
`SelectorEngine`. Extra methods beyond `RbacChecker`:

| Method | Use |
|---|---|
| `validate_selector(expression)` | compile when a selector is created or edited; return the error as a 400 (SEC-18) |
| `is_admin(principal)` | gate for administrative endpoints: not anonymous, holds the real `nx-admin` role, token unrestricted |
| `filter_visible_members(..)` | group pipeline, see above |
| `invalidate(&event)` | apply an event without the bus |

What the caller still has to do, because rbac cannot know it:

- **Anonymous gates.** Anonymous access needs `auth.anonymous_enabled` **and** the
  repository's `allow_anonymous` (CONTRIBUTING rule 4). Neither is visible to
  `RbacChecker::check`, so the HTTP layer must refuse anonymous callers before it
  calls rbac when either is off. Rbac then guarantees the rest (no anonymous write,
  only `nx-anonymous` privileges).
- **404 versus 403.** On a refusal call `can_see_repository`; false means 404.
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

`Principal::is_admin()` compares the role **name** with `nx-admin`. That is sufficient
only if the names in the principal are trustworthy, and the trap behind ADR 7 is exactly
a group that is mapped to a role reference by name. Rbac therefore asks the store for
the id of the role named `nx-admin` and matches role **ids**: a reference that claims
the name with another id is not an administrator and gets only what its id grants;
the real role is an administrator whatever name it is carried under.
(`must_not_grant_admin_from_role_name_lookalike`,
`must_not_grant_admin_to_a_role_ref_that_only_claims_the_name`.)

## Gaps in `rustryx-core`

Not edited here; listed for the owner of core.

1. **`Coordinate` has no `extension` or `classifier`.** Maven content selectors commonly
   use `coordinate.extension`. Today such an expression is a compile error. Adding
   fields to `Coordinate` and two arms in `selector/ast.rs`, `parser/mod.rs`
   (`field`) and `eval.rs` is all that is needed.
2. **`ResourceRef` carries no `allow_anonymous` and the global switch is not visible**, so
   the anonymous gates cannot live in `RbacChecker::check` (see wiring).
3. **No admin route contract in `RbacChecker`.** `Rbac::is_admin` is provided as an
   inherent method; if the api crate prefers a trait method it should be added to the
   port.
4. **`InvalidationEvent::User` carries a bare `Uuid`**, converted with `UserId::from_uuid`.
   Harmless; a `UserId` would be tidier.

## Tests

`cargo test -p rustryx-rbac` (unit and integration, plus one doctest).

| File | Covers |
|---|---|
| `tests/selector_corpus.rs` + `fixtures/selector_corpus.json` | the corpus, and that the ADR numbers match the fixture |
| `tests/selector_limits.rs` | length, depth, regex cost, input size, step budget, unknown values, anchoring, errors |
| `tests/selector_props.rs` | never panics on arbitrary text, token soup, regexes; differential test against a reference model; path canonicalization |
| `tests/rbac_decisions.rs` | roles, nesting, grants, formats, selectors |
| `tests/rbac_access.rs` | anonymous, token scopes, visibility, store errors |
| `tests/rbac_group.rs` | per member group filtering |
| `tests/rbac_cache.rs` | caching, TTL (paused time), bus invalidation, in-flight load race, listener lifecycle |
| `tests/rbac_props.rs` | anonymous never writes, scopes never extend, privileges only allow, union of roles |
| `tests/rbac_must_not.rs` | every negative requirement, tests named `must_not_*` |

The tests run against the testkit's `MemStore` and `MemInvalidationBus`, wrapped to count
store reads and to fail selector reads.
