# rustryx-format-maven

The Maven 2 repository format (`RepoFormat::Maven2`): hosted, proxy and group repositories that
`mvn`, Gradle, sbt and any other Maven repository client can deploy to and resolve from. One
`MavenHandler` implements `FormatHandler` of `rustryx-format-core` (`docs/format-core.md`); the
hosted, proxy and group pipelines, the SSRF guard, the resilience stack, name claims and the
security invariants come from there. This crate is the protocol.

Public protocol sources (no code or text of any other artifact manager was used):

* repository layout: <https://maven.apache.org/repositories/layout.html>
* `maven-metadata.xml` model: <https://maven.apache.org/ref/current/maven-repository-metadata/repository-metadata.html>
* version order: <https://maven.apache.org/pom.html#Version_Order_Specification>
* checksum algorithms and preemptive PUT authentication of the client:
  <https://maven.apache.org/resolver/configuration.html> (`aether.checksums.checksumAlgorithms`,
  `aether.transport.http.preemptivePutAuth`)
* conditional requests and ranges: RFC 9110 sections 13 and 14.

Contents: 1 protocol surface, 2 behaviour, 3 configuration, 4 limits and metrics, 5 negative
requirements and their tests, 6 ADRs, 7 deviations and open issues, 8 running the tests, 9 what
format-core and the harness should know.

## 1. Protocol surface

| Feature | Status |
|---|---|
| Layout `group/path/artifactId/version/artifactId-version[-classifier].ext`, multi dot extensions (`tar.gz`, `jar.asc`) | supported, strict (section 2.1) |
| `PUT` deploy of any file of a component, `GET`, `HEAD`, `DELETE` of a file | supported |
| `DELETE` of a whole version (`.../1.0/`, trailing slash) | supported (one transaction, immutability and hold guards apply) |
| Checksum files `.md5`, `.sha1`, `.sha256`, `.sha512` on `GET` | supported, derived from the stored file, never stored |
| Checksum files on `PUT` | supported: verified against the stored file and dropped; a mismatch is a 400 |
| `maven-metadata.xml` artifact level (versions, latest, release, lastUpdated) | supported, generated from the components on every read |
| `maven-metadata.xml` version level of a snapshot (snapshot, buildNumber, snapshotVersions) | supported, generated from the timestamped files |
| `maven-metadata.xml` group level (plugin prefixes) | proxy: passed through; group: merged by prefix; hosted: not generated (404), an upload is accepted and dropped |
| Timestamped snapshot deploys (`artifact-1.0-20240102.030405-3.jar`) and `-SNAPSHOT` resolution | supported (hosted: newest build; proxy: through the version metadata) |
| Non unique snapshots (`artifact-1.0-SNAPSHOT.jar` written in place) | supported on hosted (overwritten under `allow`); no version metadata exists for them |
| Version policy per repository (`release`, `snapshot`, `mixed`) | supported (section 3) |
| `Range`, `If-Range`, `If-None-Match`, `If-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `HEAD`, strong `ETag`, `Last-Modified`, `Content-Length` | supported on files and documents |
| Basic authentication with a `WWW-Authenticate: Basic realm="Rustryx"` challenge on 401 | supported (Maven sends credentials for `GET` only after a challenge) |
| Directory listings (`GET .../1.0/`) | not supported (404) |
| `archetype-catalog.xml`, `.meta/`, `.index/` | proxy: forwarded like any file; group: not served (section 7); hosted: served only when stored |
| Gradle module metadata (`.module`) | stored and served as any file of the component, not interpreted |
| Signatures `.asc` | stored as files of the component (extension `jar.asc`) |
| Plugin group metadata generation on hosted | not supported |
| POM parsing, parent POM checks, dependency graph | not done: a deploy never reads the POM |

## 2. Behaviour

### 2.1 Classification (before the body is read)

`classify` normalizes the path once (`NormalizedPath`, traversal in any encoding is a 400), picks
the action (`GET` and `HEAD` read, `PUT` write, `DELETE` delete; every other method is a 405 with
`Allow: GET, HEAD, PUT, DELETE`) and derives the coordinate that content selectors see:
`group`, `name` (artifactId), `version`, `extension`, `classifier`. The main artifact passes
`Some("")` as classifier (`docs/rbac.md`); metadata has no version at artifact level; a checksum file
carries the coordinate of the file it describes with the base extension, so whoever may read a file
may read its digest. Metadata documents and their checksum files are `ContentKind::Index`.

`MavenPath::parse` is strict and accepts exactly what `to_path_string` produces (`format(parse(p)) == p`).
It refuses the spellings that would give one coordinate two paths, because the write-once rule is a
rule about a path: a dot inside a group directory (`org/acme.lib/...`), a leading zero in a snapshot
build number, an extension whose first segment is only digits (`lib-1.0.1.jar` under `1.0`), a
checksum of a checksum. Known limit: a classifier cannot contain a dot (`jdk1.5` would read as
classifier `jdk1`, extension `5.jar`).

A write outside the layout is a 400. A read outside the layout is allowed through without a
coordinate (a proxy forwards it); in a group it is a 404.

### 2.2 Hosted

* **Deploy**: version policy first (a 400 before any byte is read), then `UploadSpec` with the
  component (`groupId`, `artifactId`, base version; a timestamped snapshot belongs to the component
  `1.0-SNAPSHOT`) and the repository write policy: `allow` overwrites (200), `allow_once` answers
  409 for an existing path, `deny` answers 403. The body streams to the blob store with the digests
  computed on the way (format-core); nothing is buffered.
* **Checksum files** are derived. `GET artifact.sha1` answers the digest of the stored file (MD5,
  SHA-1 and SHA-256 are on the asset row, SHA-512 is streamed from the blob in 64 KiB slices).
  `PUT artifact.sha1` is compared with the stored file in constant time and dropped; the file must
  exist (409 otherwise); a mismatch is 400 `digest_mismatch`, counted
  (`maven_checksum_checks_total{result="mismatch"}`) and audited (`asset.rejected`). A retried
  deploy therefore never trips over `allow_once` on a checksum.
* **Metadata is derived state.** Artifact level: the components of `(group, artifact)` in this
  repository, versions in Maven order, `latest` (highest, snapshots included), `release` (highest
  non snapshot), `lastUpdated` (newest change). Version level: the timestamped files of the version
  directory, the newest build, the newest file per `(classifier, extension)`. Deriving on every read
  works under `allow_once`, where the metadata port refuses a second write of any path, and is the
  same on every node. `ETag` is the SHA-256 of the document, `Last-Modified` the newest change.
  An uploaded `maven-metadata.xml` (and its checksums) is validated (UTF-8, well formed, no DOCTYPE,
  caps, names what its path names) and dropped with a 201: `mvn deploy` sends it after every
  artifact and keeps working, the repository stays the only author.
* **`-SNAPSHOT` pointer**: `GET artifact-1.0-SNAPSHOT.jar` that is not stored resolves to the newest
  timestamped build with the same classifier and extension (also for its checksum files).
* **Delete**: a file (the component goes with its last file, otherwise the generated version list
  would name a version nothing can download), or a version directory (`DELETE .../1.0/`).
  Metadata and derived checksums answer 405 with `Allow: GET, HEAD, PUT`.

### 2.3 Proxy

* Metadata is fetched as a document (bounded by `Limits::max_document_bytes`), cached for
  `metadata_max_age_secs`, revalidated by format-core (conditional request, digest comparison), served
  stale when the upstream fails, and served **byte for byte**: Maven documents have no URLs to rewrite,
  and any digest the upstream published for them stays valid. A digest the upstream announces in
  `X-Checksum-*` headers is verified (`StandardChecksumHeaders`).
* Checksum files of metadata are derived from the cached document, so they always describe the
  bytes served. The upstream's own `.sha1` is never asked for.
* Immutable files (a release, a timestamped snapshot build, and the checksum or signature files of
  either) are cached for good: once present they are served without asking the upstream,
  whatever `content_max_age_secs` says.
* A miss is verified before the first byte is served: the `.sha1` next to the file (then `.sha256`)
  is fetched as a document, parsed, and passed as the expectation of the fill. A mismatch answers 502,
  is audited by format-core and leaves nothing in the cache. No sidecar means unverified (a
  repository without sidecars is legitimate); a 403 on the probe counts as a miss, a 401 as a failure.
* `artifact-1.0-SNAPSHOT.jar` resolves through the version metadata (revalidated by the metadata
  TTL) to the timestamped build, which is immutable. Without usable metadata the file is fetched
  as it is and lives for the content TTL.
* A path that violates `version_policy` is a 404 without asking the upstream. 404 answers are
  remembered (`negative_cache_ttl_secs`); an upstream 401 or 403 is a 502, never forwarded.
* Proxied files create their component (browse, cleanup policies); sidecars and metadata do not.

### 2.4 Group

* Files: the first member that has the path answers, streamed untouched (format-core).
* `maven-metadata.xml`: every authorized member is asked and the documents are merged on the blocking
  pool. A member document that does not parse (an HTML error page, a DOCTYPE) is skipped and counted,
  it cannot take the group down; if nothing is left the answer is 422. Merge rules:
  * artifact level: union of the version lists in Maven order, `latest` and `release` recomputed,
    `lastUpdated` the newest;
  * version level (snapshot): the member with the newest build (`timestamp`, then `buildNumber`) wins as a
    whole, ties go to the earlier member; entries of two members are never mixed;
  * group level: union of `plugin` entries by prefix and artifactId, the first member wins;
  * groupId, artifactId and version of the result come from the request path.
* Checksum files of metadata are the digests of the merged bytes. An incomplete merge keeps its
  `X-Rustryx-Group-Skipped` and `Cache-Control: no-store` on the checksum too.
* **Dependency confusion**: `group_claims` returns `NameClaim::scoped(groupId, artifactId)` for every
  file, metadata and checksum request. Once a hosted member that comes earlier has any component of that
  name, later proxy members are not asked about it, for the version list and for a direct file URL.
  Put hosted members first in the group.
* Writes to a group are a 405 before the body is read (format-core).

### 2.5 Errors and headers

Errors are plain text (`text/plain; charset=utf-8`, the client safe message, `Retry-After`, `Allow`,
`Content-Range` copied from the error, `X-Request-Id`). A 401 carries `WWW-Authenticate: Basic
realm="Rustryx"`. Status mapping: policy refusals 409, `allow_once` 409, read only repository 403,
version policy 400, checksum mismatch 400, oversized body or metadata 413, unreadable metadata 422,
bad path 400, anonymous 401, no right 403, hidden repository 404.

Stored files carry `Content-Type`, `Content-Length`, strong `ETag`, `Last-Modified`, `Accept-Ranges`,
`X-Checksum-*`, `nosniff` and a sandboxing CSP (format-core). Generated and derived documents carry the
same set, built in memory with the same conditional and range logic.

## 3. Configuration

Per repository, in `format_config`:

```json
{ "version_policy": "release" }
```

`release`, `snapshot` or `mixed` (default, also when the key or the object is missing). Hosted: a write
of the other kind is a 400 with a clear text. Proxy: the other kind is a 404 without an upstream
request. An unknown value (or a non string) is a configuration fault and answers 500: a typo must not
turn a release repository into a mixed one.

Write policy comes from the repository (`write_policy`): `allow_once` is the release immutability of
SEC-37, `allow` is for snapshots, `deny` is read only. A release repository is `allow_once` plus
`version_policy: release`; a snapshot repository is `allow` plus `version_policy: snapshot`.

Per handler (`MavenConfig`): `max_metadata_bytes` (10 MiB, SEC-33), `blocking_permits` (8),
`permit_wait` (5 s).

## 4. Limits and metrics

| Limit | Value | Where |
|---|---|---|
| uploaded `maven-metadata.xml` | 10 MiB (413, also checked on `Content-Length` before reading) | `MavenConfig` |
| uploaded checksum file | 1024 bytes | `MAX_CHECKSUM_FILE_BYTES` |
| cached upstream metadata | 64 MiB | `Limits::max_document_bytes` |
| merged member documents | 64 MiB in total | `Limits::group_merge_cap_bytes` |
| XML nesting, text of one element | 12 levels, 4096 bytes | parser |
| entries per document | 100 000 versions, 10 000 snapshot versions, 10 000 plugins | parser |
| coordinate parts | 128 bytes each | layout |

Metadata is parsed on the blocking pool behind a permit taken before the spawn; without one in
`permit_wait` the client gets 503 with `Retry-After` (README 8.1). Generated metadata costs one
paged listing of the components (or of the version directory) per read.

Metrics through the facade (bounded labels only, no repository or artifact names):
`maven_metadata_ops_total{op,result}` (`generate`, `merge`, `validate_upload`, `snapshot_resolve`),
`maven_checksum_checks_total{result}`, `maven_rejections_total{reason}` (`version_policy`, `layout`).
Upload, proxy cache, singleflight and download series are format-core's.

## 5. Negative requirements and their tests

| Requirement | Test |
|---|---|
| A release is not overwritten under `allow_once` | `hosted::must_not_overwrite_a_release_under_allow_once`, `real_client::must_not_overwrite_a_release_a_second_deploy_is_a_409` |
| A read only repository accepts nothing, not even a checksum or metadata | `hosted::must_not_write_to_a_read_only_repository` |
| A wrong checksum is not accepted (all four algorithms) and is audited | `hosted::must_not_accept_a_checksum_that_does_not_match_the_file` |
| No traversal in any encoding, the authorizer never sees a bad path | `hosted::must_not_resolve_traversal_paths_and_authorization_never_sees_them`, `http::must_not_resolve_traversal_in_the_raw_request_line` |
| A coordinate has one spelling | `layout::must_not_accept_a_second_spelling_of_a_coordinate`, `layout::parse_then_format_is_the_identity_on_accepted_paths` |
| No XXE, no entity expansion | `parse::must_not_resolve_entities_defined_in_a_doctype`, `parse::must_not_accept_an_undeclared_entity`, `metadata::must_not_resolve_an_xxe_document_in_an_upload`, `http::oversized_metadata_and_xxe_are_refused_on_the_wire` |
| No billion laughs | `parse::must_not_expand_a_billion_laughs_document`, `metadata::must_not_expand_a_billion_laughs_upload` |
| Oversized metadata is refused before and while reading | `metadata::must_not_accept_oversized_metadata` |
| Metadata is never accepted unparsed | `metadata::uploaded_metadata_must_be_xml_and_must_describe_its_path` |
| A caller without the delete right deletes nothing | `hosted::must_not_delete_what_the_caller_may_not_delete` |
| A group does not leak a member the caller may not read | `group::must_not_leak_data_through_a_group_without_member_rights` |
| A proxy member does not add versions of a name a hosted member has | `group::must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has`, `group::the_claim_holds_for_a_caller_who_cannot_read_the_claiming_member`, `real_client::mvn_resolves_through_a_proxy_and_a_group_and_the_group_resists_dependency_confusion` |
| A group never accepts writes | `group::must_not_accept_writes_to_a_group` |
| A garbage member document does not break the group | `group::a_member_that_serves_garbage_is_skipped_and_cannot_take_the_group_down` |
| A file that fails its `.sha1` is neither served nor cached | `proxy::must_not_serve_or_cache_a_file_that_does_not_match_the_upstream_sha1`, `proxy::must_not_serve_a_file_that_contradicts_the_digest_in_the_upstream_headers` |
| Upstream 401 and 403 are not forwarded | `proxy::must_not_forward_an_upstream_401_or_403_to_the_client` |
| The version policy never reaches the upstream | `proxy::a_release_proxy_does_not_ask_the_upstream_for_snapshots` |
| An unknown version policy is not guessed | `config::must_not_guess_an_unknown_policy`, `hosted::an_unknown_version_policy_fails_closed` |
| No queue of blocking work | `blocking::must_not_queue_when_no_permit_is_free` |
| 401 versus 403 | `hosted::anonymous_gets_401_with_a_basic_challenge_and_an_unauthorized_user_gets_403`, `real_client::mvn_deploy_without_credentials_is_a_401_and_without_the_privilege_a_403` |
| The parsers never panic | proptests `must_not_panic_*`, `fuzz_corpus::*`, the fuzz targets (section 8) |

## 6. ADRs

1. **Metadata is generated, not stored.** Alternatives: store what the client uploads, or rewrite a stored
   document on every deploy. Storing fails under `allow_once` (the metadata port refuses a second write
   of any path, so the second deploy of an artifact could never update its metadata), races between
   concurrent deploys, and can disagree with the components. Deriving it is one listing per read and is
   identical on every node. Cost: O(versions) per read, accepted (see 7).
2. **Checksum files are derived and never stored.** A stored sidecar can disagree with its file, which is
   the failure SEC-37 describes, and cannot be overwritten under `allow_once`. SHA-512 costs a read of the
   blob (the blob store computes MD5, SHA-1 and SHA-256 only).
3. **`latest` is the highest version in Maven order, not the last deployed.** It is then the same on every
   node and a group merge can recompute it from version lists alone. The Maven order is implemented from
   the published specification and tested against its examples; it is not guaranteed a strict total order
   for adversarial text, so sorting uses a merge sort that never panics (`slice::sort_by` may).
4. **One member wins a snapshot version as a whole.** Mixing the `jar` of build 5 with the `pom` of build 3
   would describe a build no repository has.
5. **Release immutability is the repository write policy, not a hard rule.** An administrator can opt out
   with `allow` the same way as in the other formats. Snapshots are mutable only under `allow`: format-core
   treats `allow_once` as immutable for every path (see 7).
6. **Proxy releases are cached for good, snapshot pointers by the metadata TTL.** `ProxyFetch` has no per
   fetch lifetime, so the handler decides before calling the pipeline: a cached immutable file is served
   through the hosted download (it only reads the asset row and the blob), and a snapshot pointer is
   resolved through the version metadata to an immutable timestamped file.
7. **Proxy metadata is served verbatim and its checksums are derived.** A rewrite would invalidate the
   upstream's digests and signatures, and fetching the upstream sidecar separately races with a change in
   between.
8. **A group serves layout paths only.** A path without group and artifact cannot be claimed, so a proxy
   member could answer for a name a hosted member owns.
9. **Authorization of a checksum file uses the coordinate of the file it describes**, base extension
   included; a digest is derived from data the caller may already read.
10. **Errors are plain text with a Basic challenge.** Maven reads the status line; the challenge is what
    makes it retry a `GET` with credentials.

## 7. Deviations and open issues

* Group level `maven-metadata.xml` (plugin prefixes) is not generated for hosted repositories: a plugin
  deployed to a hosted repository cannot be resolved by prefix (`mvn org.acme:tool:goal` by full
  coordinates works). The upload is accepted and dropped so that `mvn deploy` of a plugin succeeds.
* Under `allow_once` snapshots cannot be overwritten (format-core refuses every second write); modern
  clients write unique timestamped files, so this only affects non unique snapshots.
* The `-SNAPSHOT` pointer reads another path than the one that was authorized (same coordinate except the
  stamp). A content selector on the exact path of the pointer would decide differently for the build.
* Generated metadata costs a listing per read; a hosted artifact with 100 000 versions is the parser's
  cap. A per node cache with a short TTL would trade freshness across nodes.
* `lastUpdated` of artifact metadata is the newest component change; it does not move when only a file
  is added to an existing component.
* A classifier cannot contain a dot.
* `If-Modified-Since` is not evaluated for merged documents and derived merged checksums (they have no
  modification time), only entity tags.
* Proxy sidecar probing costs up to two extra upstream requests on a cold miss (negative cached).
* A group does not serve `archetype-catalog.xml` or other non layout paths.
* Audit of a rejected supplied checksum is done by this crate (`FormatContext::audit_*` is crate private).

## 8. Running the tests

```
cargo test -p rustryx-format-maven --locked          # unit, proptest, contract, HTTP, corpus replay
cargo clippy -p rustryx-format-maven --all-targets --locked -- -D warnings
```

* **Contract tests** (`tests/hosted.rs`, `metadata.rs`, `proxy.rs`, `group.rs`): the real `FormatEngine` and
  `MavenHandler` over the testkit ports and a scripted upstream.
* **HTTP tests** (`tests/http.rs`): the same behind the harness HTTP listener, raw request lines for
  traversal, a wiremock upstream with 20 concurrent clients.
* **Real client tests** (`tests/real_client.rs`) run `mvn` and are off by default:

  ```
  RUSTRYX_IT=1 cargo test -p rustryx-format-maven --locked --test real_client -- --nocapture
  ```

  Without `RUSTRYX_IT=1` every test prints `SKIP: ...` and passes. The first run builds a seeded local
  repository under `CARGO_TARGET_TMPDIR/maven-it-seed` (the plugins `deploy`, `jar`, `source`, `compiler`,
  `surefire` and `dependency` need, about 30 MB, from Maven Central once, which takes a minute or two); every
  test then works on a COPY of it with `-Dmaven.repo.local`, a generated `settings.xml` with the harness token,
  and Central redirected to a dead port, so nothing touches `~/.m2` and nothing is satisfied by the internet.
  Deleting that directory forces a rebuild. The tests cover: `mvn deploy` of a jar, a pom and a sources
  classifier; `dependency:get` and `dependency:resolve` with strict checksums (`-C`) from hosted, from a proxy
  over a wiremock upstream built by Maven itself, and from a group; version ranges; unique snapshots;
  the 409, 401 and 403 a client sees; dependency confusion.
* **Fuzzing**: `fuzz/` holds five `cargo-fuzz` targets (`metadata_parse`, `layout_parse`, `version_order`,
  `merge`, `checksum_file`), each a one line call into `rustryx_format_maven::fuzzing`, with a seed corpus in
  `fuzz/corpus/<target>` (valid documents, XXE, billion laughs, truncations, binary). Running needs nightly:
  `cargo +nightly fuzz run metadata_parse -- -max_total_time=600` for each target (the nightly job: at least
  10 minutes per target, a crash blocks the release). On stable, `tests/fuzz_corpus.rs` replays every seed
  through the same entry points on every CI run, plus truncations and byte flips of the metadata seeds.
* Coverage: `CARGO_TARGET_DIR=/tmp/rustryx-cov-maven cargo llvm-cov -p rustryx-format-maven --summary-only`
  (delete the directory afterwards).

## 9. For format-core and the harness

Worked around, not changed:

* `ProxyFetch` has no per fetch freshness. "Release cached forever" needed a pre check through
  `hosted().download`, which works on a proxy repository only because it does not check the repository type.
  A `ProxyFetch::immutable()` would say it.
* `Shared::blocking` (the merge semaphore) is crate private, so a format that parses documents on its own
  (an upload to validate, a snapshot pointer) owns a second pool. A `FormatContext::run_blocking` would share it.
* `FormatContext::audit_content` and `audit_rejection` are crate private; this crate builds its own
  `AuditEvent` for a rejected checksum.
* `HashAlgo` has no `Sha512`, so a Maven `.sha512` sidecar cannot be an `ExpectedChecksum`.
* `check_write_policy` treats `allow_once` as immutable for every path, so a mixed repository cannot keep
  snapshots mutable. An `OverwriteRule::Always` would let a format say so.
* `NormalizedPath::parse` drops one leading slash, so `/repository/r//path` and `/repository/r/path` are
  the same request. Harmless (one normalized path is authorized and read) but worth knowing.
* The group's merged response has no `Last-Modified`, so `If-Modified-Since` on a merged index never gives
  304, only `If-None-Match` does.
