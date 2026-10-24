# rustryx-format-npm

The npm registry format (`RepoFormat::Npm`): hosted, proxy and group repositories that an npm
client can publish to and install from. One `NpmHandler` implements `FormatHandler` of
`rustryx-format-core` (`docs/format-core.md`); the hosted upload and download pipelines, the
proxy cache, the group fan-out, the SSRF guard, the resilience stack and the security invariants
come from there. This crate is the protocol.

Public protocol sources (no code or text of any other artifact manager was used):

* registry API: <https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md>
* package metadata, full and abbreviated: <https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md>
* legacy login: <https://github.com/npm/registry/blob/main/docs/user/authentication.md>
* package names: <https://github.com/npm/validate-npm-package-name> and <https://docs.npmjs.com/cli/v10/configuring-npm/package-json#name>
* versions: strict SemVer 2.0 (<https://semver.org>)
* the attachment payload: RFC 4648 section 4 (standard base64)

Contents: 1 protocol surface, 2 behaviour, 3 configuration, 4 limits and metrics, 5 negative
requirements and their tests, 6 decisions, 7 deviations and open issues, 8 running the tests,
9 what format-core and the harness should know.

## 1. Protocol surface

| Feature | Status |
|---|---|
| `GET` and `HEAD /<package>` packument, full (`application/json`) or abbreviated (`application/vnd.npm.install-v1+json`) | supported |
| Scoped names as `@scope/name` or `@scope%2fname` (`%2f` is a slash) | supported |
| `GET` and `HEAD /<package>/<version>` and `/<package>/<dist-tag>` | supported |
| `GET` and `HEAD /<package>/-/<name>-<version>.tgz` | supported |
| `PUT /<package>` publish (one version, tarball in `_attachments` as base64) | hosted |
| `PUT /<package>` without an attachment | hosted deprecation only (dist-tags and new versions in that body are not applied) |
| `PUT` and `DELETE /<package>/-rev/<rev>` | hosted: `PUT` unpublishes versions missing from the body; `DELETE` removes the package. `_rev` is checked for shape, not for freshness |
| `DELETE /<package>/-/<file>.tgz/-rev/<rev>` | hosted, removes that version |
| `GET /-/package/<package>/dist-tags`, `PUT` and `DELETE /-/package/<package>/dist-tags/<tag>` | hosted reads and writes; a group merges the `GET`; a proxy serves the part from the upstream document |
| `GET /-/ping`, `GET /-/whoami` | supported on every repository type |
| `PUT /-/user/org.couchdb.user:<name>`, `DELETE /-/user/token/<token>` | supported when a `TokenIssuer` is configured; otherwise `501` |
| `GET /-/v1/search` | hosted searches components; a proxy answers an empty result; a group merges member results by package name |
| `POST /-/npm/v1/security/advisories/bulk`, `.../audits`, `.../audits/quick` | supported: an empty advisory report, never an error for a missing database |
| Directory listings, CouchDB `_all_docs`, `npm star`, access documents, team routes | not part of the route table (`404`) |
| Loose versions (`v1.2.3`, `1.2`, `=1.2.3`) | refused. The client is expected to send strict SemVer |

## 2. Behaviour

### 2.1 Classification (before the body is read)

`classify` normalizes the path once (`NormalizedPath::parse_with` with `allow_encoded_slash`, so
the slash inside a scoped name survives) and parses a `Route`. Traversal in any encoding is a
`400` from the path normalizer and is never authorized. An unknown path is `404`. A known path
with the wrong method is `405` with `Allow`.

The coordinate content selectors see is the canonical (lowercase) scope as `group`, the canonical
name, and the version when the path is a tarball. A packument, a dist-tag list, a version
document and a search are index requests. Ping, whoami, login, logout and audit authorize as
`Read`. A packument `PUT` is `Write`. Setting a dist-tag is `Write`; deleting a tag, a tarball
revision or a package revision is `Delete`.

Names that would collide with the registry's own paths are not packages: a leading `-`, `.` or
`_`, `node_modules`, `favicon.ico`, more than one slash, an empty scope. `PackageName::parse`
accepts legacy spellings (capitals, `~'!()*`) so a proxy can still serve them.
`PackageName::parse_new` is what publish uses: lowercase, and those extra characters refused.

### 2.2 Hosted

* **Where bytes live.** The tarball is `<package>/-/<name>-<version>.tgz`, a component
  `(lowercase scope, lowercase name, version)`. The packument is not that path. It is a sequence
  of generations at `<package>/-/doc/<20 digit number>.json`. Readers use the highest generation.
  A change writes generation `n + 1` create-only (`OverwriteRule::Never`) and then deletes older
  generations on a best effort. A lost race re-reads and applies again, up to 16 attempts.
  Those generation paths are not routes: `GET` of them is `404`
  (`the_internal_layout_is_not_reachable_through_the_protocol`).
* **Publish.** The body is scanned, not parsed whole. Metadata is buffered up to
  `max_publish_metadata_bytes`; the base64 payload is decoded as it arrives and streamed into the
  hosted upload. The stream does not end until the JSON document is complete, so a cut-off body,
  trailing garbage, bad base64 or a SHA-512 that does not match `dist.integrity` aborts the blob
  and stores nothing. The pipeline expectation is SHA-1 (`dist.shasum`, else a `sha1-` integrity)
  and otherwise SHA-256 from integrity. SHA-512 is checked by the stream, because the blob commit
  has no SHA-512. A declared `Content-Length` that cannot fit under the blob limit (base64 expands
  by about a third, plus the metadata cap) is `413` before the body is read.
* **One version, once.** The URL package, `_id`, the single `versions` entry, its `name` and
  `version`, the attachment file name and every dist-tag must agree. Publishing over a version
  that is already present, including the same SemVer precedence with different build metadata
  (`1.0.0` and `1.0.0+build.7`), is `409`. The tarball upload is `OverwriteRule::Never` even when
  the repository write policy is `allow`, so a second publish of the same version cannot replace
  the bytes. A new version of the same package still succeeds under `allow_once`, because the
  packument is a new generation path rather than an overwrite of one document. If the document
  update fails after the tarball was stored, the tarball is removed so the client can retry.
* **What is stored in the manifest.** `dist.shasum` and `dist.integrity` become the digests the
  registry computed. `dist.tarball` is rewritten on every read to the repository URL the client
  addressed. `_npmUser.name` is the authenticated principal, not the name in the body. Hoisted
  fields (`description`, `readme`, and the rest of `HOISTED_FIELDS`) are copied from the version
  `latest` points at when that version is published. Moving `latest` later with a dist-tag `PUT`
  does not re-hoist (`retagging_latest_moves_the_hoisted_fields_only_on_publish`).
* **Read.** `ETag` is a strong tag of the stored SHA-256, the variant and the repository base URL.
  `If-None-Match` answers `304` without parsing. `Vary: Accept` and `Cache-Control: no-cache` are
  set so a publish is visible on the next read. `_rev` is `<version count>-<32 hex chars of the
  SHA-256 of the stored document>` and is the same on every node. It is not a concurrency token.
  The abbreviated document keeps the install fields (`ABBREVIATED_FIELDS`) plus `hasInstallScript`
  when the manifest says so or has `install`, `preinstall` or `postinstall`.
* **Dist-tags.** `PUT` body is a JSON string of a strict version that already exists (`404`
  otherwise, `400` for a bad body). `latest` cannot be deleted (`400`). A missing tag delete is
  `404`. Deleting a tag needs the delete privilege (`403` for a writer). After a removal, `latest`
  falls back to the highest non-prerelease that remains, else the highest version.
* **Deprecate and unpublish.** A `PUT` with no attachment applies only `deprecated` strings
  (empty or missing means not deprecated). A version in that body that the registry does not have
  is `400`: a version cannot be added without its tarball. `PUT /<package>/-rev/<rev>` removes
  versions that are no longer in the body (tarball and component together) and may also apply
  deprecations for versions that stay. `npm unpublish`'s follow-up `DELETE` of the tarball then
  finds nothing and gets `404`, which the client treats as done. `DELETE` of the package revision
  removes every component of the canonical name in this repository and the document. A package
  that was never there is `404`.
* **Search.** `text` is matched by the component query (scope, name, version). `scope:<scope>`
  filters `group_id` (a leading `@` is stripped, the value is lowercased). Other qualifiers
  (`author:`, `keywords:`, `not:`, ...) are dropped. `size` defaults to 20 and is clamped to
  `search_max_results` (default 50). `from` is an offset. `total` counts distinct packages seen
  up to `search_scan_limit` (default 2000 components). Scores are the constant `1.0`. The result
  is `{objects, total, time}`.

### 2.3 Proxy

Package documents are fetched through the format-core proxy pipeline (singleflight, metadata
TTL, conditional revalidation, negative cache, serve-stale). The full document is cached under
the package path; the abbreviated one under `<package>/-/abbreviated.json`. Each request sends
`Accept` of the variant it will render. The upstream path is the escaped package name
(`@scope%2fname`).

Before the bytes are served they are parsed leniently (invalid versions and tags are skipped)
and every `dist.tarball` is rewritten to this repository. A document larger than
`max_packument_bytes` (32 MiB) is an upstream failure (`502`), not a cached body. The rendered
`ETag` depends on the upstream validator, the variant and the repository URL.

A tarball is fetched only after the abbreviated upstream document lists that version. The
expected checksum is the SHA-256 inside `dist.integrity` when present, otherwise `dist.shasum`
or a `sha1-` integrity. The fetch goes to `<name>/-/<file>` under the configured `remote_url`.
The `dist.tarball` URL is not followed, so the document cannot point the proxy at another host.
A version that is not listed is `404` (`npm_proxy_tarball_refused_total{reason="not_listed"}`).
A listed version with no usable digest is `502`
(`reason="no_digest"`). Upstream `401` and `403` stay `502` (format-core). Search on a proxy is
an empty result and is not forwarded. Session routes are answered before the repository type is
consulted. Any other proxy route is `404`.

There is no HTTP test of a proxy repository in this crate. The rules above are what
`NpmHandler::proxied` does.

### 2.4 Group

Tarballs are the first member that has the path, streamed by format-core. Packuments, version
documents, dist-tag lists and search are index merges (`NpmMerger`).

* **Packument.** Each member body is parsed leniently. A body that is not a readable document
  (HTML, a JSON array, a cut-off object) is skipped and counted
  (`npm_group_merge_total{result="skipped_member"}`). If nothing remains the answer is `422`.
  Versions are the union; a version present in several members comes from the earlier member.
  A dist-tag that a claim-holding member has comes from the first such member; any other tag
  takes the highest version among members that have it. `time.created` is the earliest,
  `time.modified` the latest, and a version's own time comes from the member that won that
  version. Other top-level fields come from the claim holder, else the first member. `latest`
  is repaired onto a version that exists, preferring a release over a prerelease. The merger
  does not rewrite `dist.tarball` itself; it keeps the `dist` object of the winning version.
* **Version document.** The first member body that is a JSON object is returned unchanged.
  Non-objects are skipped. If none parse, `422`.
* **Dist-tags.** JSON values are read leniently and merged with the same tag rule. A body that
  is not JSON is skipped. If none parse, `422`.
* **Search.** `objects` are concatenated. The first member to name a package wins. Bodies that
  are not a JSON object with an `objects` array are skipped. `total` is the number of objects
  kept. A search that names no package is not a name claim.
* **Claims.** Every route that names a package claims the lowercase scope and name
  (`NameClaim::scoped` or `NameClaim::package`). `@Acme/Billing` and
  `@Acme/Billing/-/Billing-1.0.0.tgz` both claim `acme` / `billing`, the form components are
  stored in. Ping and search claim nothing. Format-core then skips later non-hosted members
  when an earlier hosted member holds that name. This crate does not implement that lookup; it
  only reports the claim. Writes to a group are `405` from the engine before `grouped` runs.

The merge and claim rules have unit tests. There is no test-server group repository in this crate.

### 2.5 Session

`GET /-/ping` is `{}`. `GET /-/whoami` is `{"username": ...}` for an authenticated caller and
`401` for anonymous (`npm whoami` must not print a name).

`PUT /-/user/org.couchdb.user:<user>` issues a token only when all of these hold: the caller is
already authenticated with a password (`BasicLocal` or `BasicLdap`), the URL user is that
principal, the body is JSON no larger than 16 KiB, and if `name` is present it equals the URL
user. The password field is not used and not logged. A token-authenticated caller (API token or
OIDC) is `403`: a token must not mint another token. Anonymous is `401`. A document that is not
JSON or names someone else is `400`. With no `TokenIssuer`, login and logout answer `501` and
`{"code":"not_configured"}`. A successful login is `201` with `ok`, `id`, a fixed `rev` string
and `token`. Logout deletes through the issuer and is `{"ok":true}`.

Audit `POST`s ignore the body (callers send gzip) and answer `200`. The bulk route is `{}`. The
quick and full audit routes are a report whose advisory map is empty and whose vulnerability
counts are zero.

These login and logout rules are implemented and are not covered by an HTTP test.

### 2.6 Errors and headers

Errors are JSON: `error` and `message` are both `FormatError::public_message`, and `code` is the
stable code. `Retry-After`, `Allow` and `Content-Range` are copied from the error. A `401` sets
`WWW-Authenticate: Bearer realm="rustryx", Basic realm="rustryx"`. A `403` does not. Status
mapping is format-core's: bad publish or digest mismatch `400`, anonymous `401`, authenticated
without the privilege `403`, hidden repository `404`, conflict `409`, oversized body `413`,
unreadable group merge `422`, no blocking permit `503`.

Publish and dist-tag writes that succeed answer `{"ok":true,"id":...,"rev":...}`. `HEAD` keeps
the headers and drops the body.

## 3. Configuration

There is no `format_config` object. Limits are `NpmConfig` on the handler:

| Field | Default |
|---|---|
| `max_packument_bytes` | 32 MiB (stored reads and upstream documents) |
| `max_publish_metadata_bytes` | 16 MiB (everything in a publish except the tarball payload) |
| `blocking_permits` | 4 |
| `search_scan_limit` | 2000 components |
| `search_max_results` | 50 |

`NpmHandler::with_token_issuer` is optional. Without it the legacy login routes answer `501`.

Repository write policy does not make a tarball replaceable. `deny` still refuses hosted writes
and deletes through format-core. `allow_once` does not block a second version, because the
packument path changes (section 2.2).

## 4. Limits and metrics

| Limit | Value | Where |
|---|---|---|
| packument bytes | 32 MiB | `NpmConfig::max_packument_bytes` |
| publish metadata | 16 MiB | `NpmConfig::max_publish_metadata_bytes`; `413` when the scan crosses it |
| versions in one document | 100 000 | `MAX_VERSIONS` |
| dist-tags | 256 | `MAX_TAGS` |
| package name, including `@scope/` | 214 bytes | `MAX_NAME_LEN` |
| version text | 256 bytes | `MAX_VERSION_LEN` |
| dist-tag name | 128 bytes | `MAX_TAG_LEN` |
| `dist.integrity` text | 1024 bytes | `MAX_INTEGRITY_LEN` |
| JSON nesting while scanning a publish | 64 | `ScanLimits::max_depth` |
| object key tracked at the top levels | 1024 bytes | `ScanLimits::max_key_bytes` |
| dist-tag `PUT` body | 1024 bytes | `MAX_TAG_BODY` |
| login body | 16 KiB | `MAX_LOGIN_BODY` |
| document CAS attempts | 16 | `docstore::MAX_ATTEMPTS` |
| generation listing | 100 per page, 50 pages | `docstore` |
| unpublish component listing | 200 per page, 500 pages | `unpublish` |
| search page | 200 components | `search::SCAN_PAGE` |

Parsing runs on `spawn_blocking` after a permit is taken. If none is free within the format-core
permit wait, the client gets `503` and `Retry-After: 1`. A panic in that task is `500`.

Metrics (bounded labels only, no package names): `npm_publish_total{result}`,
`npm_publish_bytes_total`, `npm_document_cas_retry_total`, `npm_dist_tag_changes_total{op}`
(`set`, `remove`), `npm_unpublish_total{scope}` (`version`, `package`),
`npm_packument_served_total{source,variant}`, `npm_group_merge_total{result}` (`ok`,
`skipped_member`), `npm_proxy_tarball_refused_total{reason}` (`not_listed`, `no_digest`),
`npm_session_total{op,result}`.

## 5. Negative requirements and their tests

| Requirement | Test |
|---|---|
| A published version is not replaced, including under `allow` and `allow_once`, and build metadata is not a second release | `hosted::must_not_replace_a_published_version`, `hosted::a_version_stays_immutable_under_every_write_policy`, `packument::build_metadata_does_not_open_a_second_release` |
| A wrong SHA-512, SHA-1 or SHA-256 stores nothing | `hosted::must_not_store_anything_for_a_wrong_digest`, `attach::must_not_end_normally_when_the_sha512_does_not_match` |
| A cut-off body, bad base64, trailing garbage or a document for another package stores nothing | `hosted::must_not_accept_a_malformed_attachment`, `hosted::must_not_accept_a_truncated_or_broken_body`, `hosted::must_not_accept_a_document_that_does_not_belong_to_the_url`, `publish::must_not_accept_a_document_for_another_package`, `scan::must_not_accept_*`, `attach::must_not_end_normally_*` |
| Oversized publish metadata is refused and stores nothing | `hosted::must_not_accept_an_oversized_metadata_part`, `scan::must_not_exceed_the_metadata_limit` |
| No traversal in any encoding, and a tarball whose archive names escape is stored as bytes and never extracted | `hosted::must_not_resolve_traversal_in_any_encoding`, `hosted::a_tarball_with_a_traversal_name_is_stored_as_is_and_never_opened` |
| Anonymous is `401` with a challenge; a reader who cannot write is `403`; a user with no privilege on the repository is `404`; the body is not applied | `hosted::authentication_and_authorization_are_checked_before_the_body_is_read`, `hosted::a_401_names_a_challenge_and_an_error_body_is_the_registrys` |
| Generation paths and other non-routes are not served | `hosted::the_internal_layout_is_not_reachable_through_the_protocol`, `route::must_not_route_names_that_collide_with_the_registry`, `route::unknown_api_paths_are_404_not_a_package` |
| `latest` cannot be removed; a tag that is a version is not a tag; a dist-tag cannot point at a missing version | `hosted::dist_tags_can_be_listed_set_and_removed`, `disttags::set_validates_and_bounds`, `version::tests::tag_rules`, `version::tests::strict_semver_only` |
| A deprecation body cannot add a version or target another package | `publish::must_not_apply_a_document_of_another_package_or_a_malformed_one`, `hosted::deprecate_writes_the_document_back` |
| More than one version per publish, a non-SemVer version, or a digest that is not well formed, is refused | `publish::must_not_accept_more_or_fewer_than_one_version`, `publish::must_not_accept_a_version_that_is_not_semver_or_disagrees_with_its_key`, `publish::must_not_accept_malformed_digest_claims` |
| A publish document cannot smuggle structure with escapes in the keys that matter, duplicate keys, or a second attachment | `scan::must_not_accept_escapes_in_the_keys_that_matter`, `scan::must_not_accept_duplicate_top_level_keys`, `scan::must_not_accept_a_second_attachment_or_a_second_data`, `scan::must_not_accept_decisions_after_the_attachment` |
| A packument cannot carry more than the version cap, and a deep document is an error | `packument::must_not_accept_more_versions_than_the_limit`, `scan::must_not_nest_beyond_the_depth_limit` |
| A garbage member document does not take a group merge down, and a tarball request is not merged | `group::must_not_let_a_garbage_member_take_the_group_down`, `group::must_not_merge_a_request_that_is_not_a_document` |
| A mixed-case name does not claim a different coordinate than the lowercase form components use, and search and ping claim nothing | `group::must_not_claim_a_lookalike_spelling_or_a_route_without_a_package`, `name::canonical_lowercases_both_parts_and_is_idempotent`, `route::the_coordinate_is_scope_name_and_version_in_lowercase` |
| The blocking pool does not queue work with no permit | `blocking::must_not_queue_without_a_permit` |
| The parsers do not panic on arbitrary input | proptests in `packument`, `publish`, `scan` (`must_not_panic_on_arbitrary_documents`, `never_panics_*`) |

## 6. Decisions

1. **The packument is a new generation, not an overwrite.** `allow_once` would refuse a second
   write of one path, and two publishers who both overwrite generation `n` would drop a version.
   Create-only on `n + 1` makes the database the compare-and-set. Cost: one listing per read, and
   a blob per change until the old generation is deleted.
2. **A tarball is immutable under every write policy.** `OverwriteRule::Never` is set on the
   upload. Replacing `1.0.0` is a `409` even on an `allow` repository. Snapshot-style overwrite
   is not an npm operation this crate implements.
3. **The publish body is split before it is parsed.** Holding the base64 text, the decoded bytes
   and a `serde_json::Value` at once would multiply a large tarball. The scanner is exact about
   strings and refuses escapes in the keys it has to understand, duplicate top-level keys, and
   anything after `_attachments` that would change a decision already taken.
4. **`_rev` is derived, not checked.** Clients send it because CouchDB did. Concurrency is the
   generation number. A stale document can still unpublish a version that is absent from that
   snapshot; it cannot resurrect a tarball.
5. **Metadata from an upstream or a group member is lenient; metadata this registry writes is
   strict.** One bad version on a public packument must not hide the package. A document this
   process stored that fails to parse is `500`.
6. **Proxy tarball fetches do not follow `dist.tarball`.** The path under `remote_url` is the
   only upstream URL. A digest the blob store can check (SHA-256, else SHA-1) is required.
7. **Group tag merge prefers the claim holder.** A public proxy must not move `latest` of a name
   a hosted member owns. Tags nobody claimed take the highest version. Version bytes of a shared
   version come from the earlier member, so `dist` and the tarball lookup agree.
8. **Audit is an empty success.** Failing `npm install` because this registry has no advisory
   database would be a false alarm. The body says nothing is known.
9. **Login does not check the password in the body.** The HTTP layer already authenticated a
   password login. A bearer token cannot call this route to mint another token.

## 7. Deviations and open issues

* No `format_config`. No per-repository version policy.
* Search does not rank, does not apply qualifiers other than `scope:`, and stops after
  `search_scan_limit` components. A proxy search is empty rather than forwarded.
* Login and logout need a `TokenIssuer` supplied by the composition root. Without one they are
  `501`. Keycloak users are expected to create an API token in the UI. These routes have no HTTP
  test.
* `_rev` does not reject a stale write. Two concurrent unpublishes are serialized by the
  generation compare-and-set, but the set of versions removed is whatever each body listed.
* A failed rollback after a publish that stored a tarball and then could not update the document
  leaves the component in place until an administrator deletes it. The error is logged.
* Old generations are deleted on a best effort. A refused delete leaves a stale generation that
  readers ignore.
* Unpublish of a package reads at most 500 pages of 200 components. A package beyond that is only
  partly removed by one call.
* The group merger does not rewrite tarball URLs. It assumes member documents were rendered for
  the group origin. Version documents are copied from the first JSON object, not re-serialized.
* Proxy and group behaviour is not covered by `tests/hosted.rs`. The unit tests cover merge,
  claims, packument merge and the parsers.
* There is no real `npm` client test in this crate, and nothing reads `RUSTRYX_IT`.
  `tests/hosted.rs` mentions `real_clients.rs`; that file is not in the tree.
* `latest` on a hosted package cannot be deleted. The public registry allows deleting it in some
  cases; this crate repairs or refuses instead, so `npm install <package>` keeps a target.
* Abbreviated and full documents are cached separately on a proxy. A group merge builds one
  variant from the member bodies it was given; it does not fetch the other variant itself.

## 8. Running the tests

```
cargo fmt -p rustryx-format-npm
cargo clippy -p rustryx-format-npm --all-targets --locked -- -D warnings
cargo test -p rustryx-format-npm --all-targets --locked
```

* **Unit tests** live next to the parsers (`name`, `version`, `packument`, `disttags`, `publish`,
  `scan`, `attach`, `route`, `respond`, `variant`, `blocking`) and in `src/group/tests.rs`
  (merge and claims). Several are `proptest` and must not panic.
* **HTTP tests** (`tests/hosted.rs`, helpers in `tests/support/mod.rs`) run `NpmHandler` on
  `rustryx-testserver`: publish, read, dist-tags, deprecate, unpublish, search, auth, traversal.
  They build publish bodies in the key order `npm publish` uses. They do not start `npm`.
* Do not set `RUSTRYX_IT` for this crate. No test invokes a real client, and a test that needed
  one would have to skip unless that variable were set. None does.

## 9. For format-core and the harness

Worked around, not changed:

* The engine's blocking semaphore is private, so publish parsing, packument rendering and proxy
  document checks use a second pool (`NpmHandler`'s `Blocking`). A `FormatContext::run_blocking`
  would share the merge pool.
* `HashAlgo` has no SHA-512. Publish checks SHA-512 in `AttachmentStream`. A proxy can only
  require SHA-256 (from `dist.integrity`) or SHA-1 (`dist.shasum`). A tarball that publishes only
  a SHA-512 integrity is refused (`no_digest`) rather than fetched unverified.
* `OverwriteRule::Never` is how a version stays immutable on an `allow` repository. The packument
  stays writable under `allow_once` only because each generation is a new path.
* Group name claiming is format-core's lookup. This crate's `NpmClaims` returns the lowercase
  scope and name. A claim that used the raw spelling would miss the stored component.
* `dist.tarball` on a proxied or hosted read is rewritten to
  `RequestHead::repository_url`. The harness base URL is what clients are told to fetch.
