# rustryx-format-pub

The Dart and Flutter package format (`RepoFormat::Pub`): hosted, proxy and group repositories that speak the Hosted Pub Repository Specification Version 2. One `PubHandler` implements `FormatHandler` of `rustryx-format-core` (`docs/format-core.md`). `dart pub` and `flutter pub` use the same client, so there is no second protocol. The hosted, proxy and group pipelines, the SSRF guard, the resilience stack, name claims and the security invariants come from format-core. This crate is the protocol.

Public protocol source (no code or text of any other artifact manager was used):

* repository specification: <https://github.com/dart-lang/pub/blob/master/doc/repository-spec-v2.md>

Contents: 1 protocol surface, 2 behaviour, 3 configuration, 4 limits and metrics, 5 negative requirements and their tests, 6 ADRs, 7 deviations and open issues, 8 running the tests, 9 what format-core and the harness should know.

## 1. Protocol surface

| Feature | Status |
|---|---|
| `GET` and `HEAD` `api/packages/<package>` (version listing) | supported |
| `GET` and `HEAD` `api/packages/<package>/versions/<version>` (deprecated single version, Dart before 2.8) | supported, derived from the same data as the listing |
| `GET` and `HEAD` `packages/<package>/versions/<version>.tar.gz` (archive, also `archive_url`) | supported |
| `GET api/packages/versions/new` (publish step 1) | supported on hosted; 405 on proxy and group |
| `POST api/packages/versions/upload/<id>` and optional `/<package>/<version>` (publish step 2) | supported on hosted; 405 on proxy and group |
| `GET api/packages/versions/finish/<id>` and optional `/<package>/<version>` (publish step 3) | supported on hosted; 405 on proxy and group |
| Optional `?package=&version=` on step 1, echoed into the upload and finish paths | supported: both parameters or neither |
| `Accept: application/vnd.pub.v2+json` on JSON routes | supported; a missing `Accept` means version 2; another version alone is 406 |
| Listing fields `name`, `latest`, `versions`, `isDiscontinued`, `replacedBy` | supported as specified in section 2 |
| Entry fields `version`, `retracted`, `archive_url`, `archive_sha256`, `pubspec`, `published` | supported; `published` is an extension clients ignore |
| `archive_sha256` checked on a proxy archive fetch | supported when the listing has a digest; a version without one is served unverified |
| Package names and canonical SemVer 2.0.0 versions | supported, strict (section 2.1) |
| Bearer challenge on 401 and 403 | supported (section 2.5) |
| `HEAD`, strong `ETag`, `If-None-Match`, `Content-Length`, `Content-Type` on generated JSON | supported |
| Advisories `GET api/packages/<package>/advisories` and `advisoriesUpdated` | not supported (404; the field is never forwarded) |
| Retract or discontinue on a hosted publish | not supported (hosted entries are stored with `retracted: false` and no package-level discontinuation) |
| `PUT`, `PATCH`, `DELETE` of a published path | not supported (405, `Allow: GET, HEAD`) |
| Directory listings, query other than the publish declaration | not part of the routes |
| A real `dart pub` process | not run by this crate (section 8) |

## 2. Behaviour

### 2.1 Classification (before the body is read)

`classify` normalizes the path once (`NormalizedPath`; traversal in any encoding is a 400) and parses a `Route` from that value. `handle` and the group claimer parse the same path again, so the authorized path, the claimed name and the served route are one value. `Route::parse` accepts exactly what `to_path` produces.

Routes:

```text
GET  api/packages/<package>
GET  api/packages/<package>/versions/<version>
GET  api/packages/<package>/advisories
GET  packages/<package>/versions/<version>.tar.gz
GET  api/packages/versions/new
POST api/packages/versions/upload/<id>[/<package>/<version>]
GET  api/packages/versions/finish/<id>[/<package>/<version>]
```

A package may be named `versions`. That name is the three-segment list `api/packages/versions`. Publish routes need a fourth segment (`new`, `upload` or `finish`). `new` is not a SemVer, so `api/packages/<package>/versions/new` is a bad version (400), not the publish route.

An upload id is a canonical lowercase hyphenated UUID. Upper case, missing hyphens, braces and a `urn:` prefix are 400. A bad package name or version segment is 400. A path outside the protocol is 404.

Reads (`GET` and `HEAD`) of a list, a version, an archive or advisories need `Action::Read`. `GET` of `versions/new` and of finish need `Action::Write` (they change state; `HEAD` is 405 with `Allow: GET`). Upload is `POST` only (`Allow: POST`). Every other method on a read route is 405 with `Allow: GET, HEAD`.

The coordinate content selectors see has no group. A list and the advisories route name the package only. A version and an archive name the package and the version. List and version routes are `ContentKind::Index`. A standard client does not declare a package on the publish steps, so those steps carry no coordinate until the archive is read. A client that sends both `package` and `version` (step 1 query, or the extra path segments on upload and finish) gets that coordinate before the body is read. One of the two parameters without the other is 400.

JSON routes (list, single version, `versions/new`, finish) require an acceptable `Accept`. Version 2, `application/json`, `application/*` and `*/*` are accepted. Quality values are not interpreted. A header that is not text is ignored. A request that names only other types (for example `application/vnd.pub.v3+json`) is 406 `not_acceptable`. The archive is a file and is not negotiated. Advisories are not negotiated; they are 404.

### 2.2 Hosted

Publication is three steps. Nothing in the archive is trusted until step 3.

1. `GET api/packages/versions/new` answers `{"url":"<repository>/api/packages/versions/upload/<id>","fields":{}}` with `Cache-Control: no-store`. A read-only write policy (`WritePolicy::Deny`) is 403 before any upload URL is issued. Starting an upload also schedules reaping of staged uploads older than `staging_ttl` (one batch, not awaited on the request).
2. `POST` the multipart body. The only allowed field is `file`. The file part is cut out of the stream and stored through format-core at the request path itself (`api/packages/versions/upload/<id>...`), content type `application/gzip`, create only. The answer is 204 with `Location` pointing at the matching finish URL. A second POST to the same id is 409 and its body is not read. A `Content-Length` above `max_upload_bytes` plus 64 KiB of multipart overhead is 413 before a byte is read. A missing boundary, an unexpected field, more than one file part, or a truncated multipart body is 400, and a copy that was staged for a body that then fails is deleted.
3. `GET` finish runs as a task tracked by the engine, so a client that disconnects does not cancel it. The staged asset must exist (404 if it does not; unknown, finished and reaped ids look the same) and `created_by` must be the caller (403 otherwise; the copy is left in place). The archive is then read from the blob store on the blocking pool. The manifest name and version must equal a declared coordinate when one was given (400 `declared_mismatch` otherwise). The caller is authorized again for `Action::Write` on that exact coordinate. Deny is 403. An authorization backend error is 403, never an allow, except `AuthError::Overloaded`, which is 503 with `Retry-After: 1`. The policy hook runs, then the archive path `packages/<package>/versions/<version>.tar.gz` is published create-only as a link to the staged blob, on the component `(package, version)`. A second publication of that version is 409, including under `allow` and `allow_once`. The version document is then stored at `api/packages/<package>/versions/<version>`. If that write fails, the archive asset is removed. The staging copy is deleted. The success body is `{"success":{"message":"Successfully uploaded <package> version <version>."}}`.

`allow_once` still accepts a new version of a package. The same version is never replaced: immutability is create-only, not the repository write policy. A store in read-only mode refuses the upload with 503 and `Retry-After` and keeps nothing staged.

What is stored per version:

* the archive, linked to the blob the upload already wrote;
* a JSON document with `version`, `retracted` (always false), `archive_sha256` (the asset SHA-256), `pubspec` (the manifest as JSON) and `published` (RFC 3339 of the handler clock). `archive_url` is not stored.

The listing reads those documents (paged, `version_read_concurrency` at a time), drops one that is not JSON or does not describe the version in its path, and joins them. `archive_url` is `<repository the client addressed>/packages/<package>/versions/<version>.tar.gz`. Versions are unique and sorted ascending by SemVer precedence, build metadata included (`1.0.0+1` and `1.0.0+2` are two versions). `latest` is the highest stable version that is not retracted, else the highest pre-release that is not retracted, else the highest version at all. A hosted publish never sets `retracted` or `isDiscontinued`. An empty listing is 404. More versions than `max_versions_per_package`, or stored documents whose sizes sum past format-core `Limits::max_document_bytes`, is a 500 rather than a truncated list. The single-version route is that one entry. The archive route is `hosted().download`.

Deleting the component through the component port removes the archive and the version document together. The component is also what a hosted group member claims the package name with.

The archive reader (`archive::inspect`) runs on the whole gzip member, not only up to `pubspec.yaml`. It refuses: a stream that is not gzip-compressed tar; a bad tar checksum; a bad gzip trailer; data after the end-of-archive marker that is not zero padding; data after the gzip member; too many decompressed bytes; an expansion ratio above `max_ratio` once `ratio_floor_bytes` have been decompressed; too many entries; an entry or a manifest over its cap (the size claimed in the header is enough; the bytes need not be present); GNU long names over 4096 bytes and PAX blocks over 64 KiB; a repeated or dangling extension header; a PAX `size` (this reader and an extractor would disagree about the next header); symlinks, hard links, devices and any entry that is not a regular file or a directory; a path that is not UTF-8, is absolute, has a drive prefix, a backslash, a control character, an empty segment, `.` or `..`, a segment over 255 bytes, or a path over `max_path_bytes`. A single leading `./` and a root directory entry are accepted. `pubspec.yaml` must be one regular file at the root. A second copy, a nested `pubspec.yaml` used as the only manifest, and a case variant such as `PUBSPEC.yaml` are refused.

Limits on the archive are 422 (`bomb_ratio` and the other size limits). A malformed archive, an unsafe path, an unsupported entry, a bad manifest placement and trailing data are 400. The manifest parser is 422 for size, alias-bomb complexity, depth and over-long strings, and 400 for syntax, ambiguity, a missing or invalid name or version, and a non-mapping document. A UTF-8 BOM is stripped. A YAML number (`version: 1.0`) is not a version string. Duplicate keys, non-string keys, non-finite numbers and custom tags are refused. Alias expansion is charged per value the visitor is handed, so an alias bomb stops at `max_nodes`.

### 2.3 Proxy

The upstream is a hosted pub repository (the public default is `https://pub.dev`, configured on the repository, not in this crate).

The listing is fetched from `<remote>/api/packages/<package>` with `Accept: application/vnd.pub.v2+json` through format-core's cached document path (metadata TTL, singleflight, negative cache, stale serving, SSRF guard, retries). The cached bytes are the upstream document. Each request rebuilds the client document: `archive_url` becomes `<repository the client addressed>/packages/<package>/versions/<version>.tar.gz`, and entries that cannot be trusted are dropped and counted (`pub_listing_entries_dropped_total`). Dropped means a version that is not canonical SemVer, a pubspec that is not an object or names another package or version, a retracted value that is not a boolean, a digest that is not 64 hex digits, or a repeated version (the first wins). `published` longer than 64 bytes is omitted. `advisoriesUpdated` and any other field are not copied. `isDiscontinued` is kept only when the JSON boolean is true. `replacedBy` is kept only then, and only when it is a valid package name. A digest is stored lowercase. If every entry is dropped, or the upstream has no versions array the parser can use, the client gets 404 when the document was readable and empty, and 502 when the document is not a listing of this package. The 502 body does not contain upstream text. A package the upstream does not have is remembered (one upstream call for repeated lookups in the negative-cache window).

The deprecated single-version document is derived from that same cached listing, so the proxy does not store a second document. A version the listing does not contain is 404.

The archive is fetched only for a version the listing contains, from `<remote>/packages/<package>/versions/<version>.tar.gz` (the path the specification requires servers to keep; pub.dev answers it with a redirect). The format-core executor follows redirects up to its configured maximum (3) and re-validates every hop. When the listing has `archive_sha256`, that digest is the expected SHA-256: a mismatch is 502, nothing is cached, and nothing is sent. A version with no digest is fetched without an expected checksum and is listed without `archive_sha256`. An upstream 401 or 403, on the listing or the archive, is 502 with no `WWW-Authenticate` and is not described as unauthenticated. An upstream that cannot be connected to, with no cached listing, is 502. A cached listing past its freshness is served with `x-rustryx-cache: STALE` and `Warning: 110` when the upstream then fails.

Publication routes on a proxy are 405 with `Allow: GET, HEAD`. The body is not read and the upstream is not called. A caller with no read right is 403 (anonymous is 401) and causes no upstream request.

### 2.4 Group

Reads of a list, a version or an archive go through `FormatContext::group`. Writes and the publish routes are 405 with `Allow: GET, HEAD`. Advisories are 404.

`PubClaimer` claims the package name of every read route, including advisories. It does not claim a publish route. Format-core applies the claim: a hosted member that already has the package, and that is ordered before a proxy member, stops the pipeline from asking that proxy. The public version of an internal name therefore does not appear in the listing, the single-version document, or the archive, and the upstream is not called. The claim still holds for a caller who may not read the hosted member: that caller gets 404, not the public look-alike. A hosted member ordered after a proxy does not pin against that proxy; both versions are merged. Hosted members belong first.

`PubMerger` unions list documents by exact `PubVersion` (build metadata included), sorted ascending. The first member in group order wins a duplicate version, and the entry is copied whole, so `retracted` and `archive_sha256` stay with the bytes that member serves. `isDiscontinued` and `replacedBy` come from the member that holds the claim, otherwise from the first member. `latest` is recomputed. `archive_url` values in a member listing are kept; handlers rewrite them to the repository the client addressed before the merger sees them, including when that repository is an outer group.

A single-version document is not merged field by field. The first member document is the answer, which is the first member that has that version.

A member listing that is not readable JSON for this package fails the merge with 422. The body does not quote the member payload. `pub_merge_documents_skipped_total` is incremented for that refusal. A member that is down is format-core's skip: the listing can still be 200, with `x-rustryx-group-skipped` set and `Cache-Control: no-store`. A hosted member whose authorization lookup fails is 403 for a caller who has a token, never 401 and never 404. A proxy member whose upstream answers 401 or 403 is 502, not a challenge and not `x-rustryx-group-skipped`.

A caller who may read the group but not the member that holds a package gets 404 for the list, the version and the archive. A caller with no right on the group itself gets 403. An anonymous caller gets 401.

### 2.5 Errors and headers

Every error this handler renders is `Content-Type: application/vnd.pub.v2+json`, `X-Content-Type-Options: nosniff`, and a body `{"error":{"code":"...","message":"..."}}`. The generic API envelope is not used. Headers already attached to the `FormatError` (for example `Allow`, `Retry-After`) are copied.

Status rules that matter to the client:

| Situation | Status |
|---|---|
| No credentials | 401 `unauthenticated` |
| Valid token, missing right, hidden repository, write policy deny, foreign upload, authorization backend failure | 403 `forbidden` |
| A 401 produced for a caller who is not anonymous | rewritten to 403 before it is sent |
| Unknown path, unknown package or version, advisories, unknown upload id | 404 |
| Bad name, bad version, bad upload id, half a declaration, manifest or archive rule that is not a size limit, declared coordinate mismatch | 400 |
| `Accept` names no type this repository serves | 406 `not_acceptable` |
| Version already published, upload id already received | 409 `conflict` |
| Declared or actual body over the upload limit | 413 |
| Archive or manifest over a complexity or size limit | 422 |
| Method the route does not allow, including every publish route on a proxy or a group | 405 |
| Read-only storage, no blocking permit, authorization overloaded | 503 with `Retry-After` |
| Upstream failure, checksum mismatch, unusable upstream listing | 502 |

401 and 403 carry `WWW-Authenticate: Bearer realm="pub", message="<message>"`. No other status does. The 401 message is fixed: `Authentication is required. Create an API token and add it with dart pub token add.` The 403 message is fixed: `Access denied. The credentials are valid but do not allow this operation on this repository.` The challenge keeps only printable ASCII, and replaces quotes, backslashes and other characters with spaces. `hide_inaccessible_repository` is false, so a repository the caller may not see is 403, not 404.

Generated JSON documents carry the media type, `Content-Length`, `nosniff` and a strong `ETag` (quoted SHA-256 of the exact bytes sent). A matching `If-None-Match` is 304 with an empty body and that `ETag`. `HEAD` has the headers and an empty body. The `ETag` is computed after `archive_url` is rewritten, so it is not the upstream validator. Publish answers add `Cache-Control: no-store`. Proxy documents also get `x-rustryx-cache`, and `Warning: 110` when the copy is stale. Archive responses are format-core downloads; the hosted contract test checks the body, `Content-Length` and `x-checksum-sha256`.

## 3. Configuration

There is no per-repository `format_config`. The handler is built with `PubConfig` (the defaults below). The repository supplies `write_policy` and `allow_anonymous`.

`WritePolicy::Deny` refuses step 1 with 403. `Allow` and `AllowOnce` both accept new versions. Neither can replace an existing version. Anonymous reads succeed only when the repository allows anonymous and the anonymous principal has read. Anonymous writes are 401 even then.

| `PubConfig` field | Default |
|---|---|
| `max_upload_bytes` | 100 MiB |
| `archive.max_uncompressed_bytes` | 256 MiB |
| `archive.max_entries` | 50 000 |
| `archive.max_entry_bytes` | 128 MiB |
| `archive.max_pubspec_bytes` | 256 KiB |
| `archive.max_path_bytes` | 1024 |
| `archive.max_ratio` | 100 |
| `archive.ratio_floor_bytes` | 4 MiB |
| `pubspec.max_bytes` | 256 KiB |
| `pubspec.max_nodes` | 50 000 |
| `pubspec.max_depth` | 32 |
| `pubspec.max_string_bytes` | 64 KiB |
| `blocking_permits` | twice the reported parallelism, and at least 1 inside the pool |
| `blocking_permit_wait` | 5 s |
| `staging_ttl` | 24 h |
| `max_versions_per_package` | 10 000 |
| `max_version_document_bytes` | 1 MiB |
| `version_read_concurrency` | 8 |
| `reap_batch` | 100 |

Package names are 1 to 64 bytes: a lowercase ASCII letter, then lowercase letters, digits and underscores. Nothing is case-folded. Versions are at most 128 bytes and must be canonical SemVer 2.0.0 (`semver` crate): no leading zeros, no `v` prefix, no spaces. Build metadata is significant for equality and order.

## 4. Limits and metrics

| Limit | Value | Where |
|---|---|---|
| Compressed upload | 100 MiB, plus 64 KiB allowed for multipart framing on `Content-Length` | `PubConfig::max_upload_bytes` |
| Decompressed archive, one entry, manifest, path, entries, ratio | section 3 | `ArchiveLimits` |
| GNU long name, PAX block, one path segment | 4096 bytes, 64 KiB, 255 bytes | archive reader |
| Manifest nodes, depth, strings | section 3 | `PubspecLimits` |
| Versions of one package | 10 000 | hosted listing, proxy parse, group merge |
| One stored version document | 1 MiB | hosted read |
| Sum of version documents in one hosted listing | format-core `max_document_bytes` | hosted read |
| Blocking bridge | 2 chunks in flight | `BlockingPool` |
| Reap page | 200 assets | reaper |

A permit is taken before `spawn_blocking`. If none is free within `blocking_permit_wait`, the client gets 503 and `Retry-After: 1`, and `pub_blocking_refused_total` increments. No queue of blocked work is built. A panicking blocking task is a 500.

Metrics through the `metrics` facade (bounded labels only, no package names):

| Metric | When |
|---|---|
| `pub_publish_total{result}` | finish: `ok`, or a fixed reason (`version_exists`, `forbidden`, `bomb_ratio`, `pubspec_name`, `declared_mismatch`, ...) |
| `pub_listing_entries_dropped_total` | proxy entries left out of the rewritten listing |
| `pub_blocking_refused_total` | no blocking permit in time |
| `pub_staged_uploads_reaped_total` | staged uploads removed because they were older than `staging_ttl` |
| `pub_merge_documents_skipped_total` | a member listing the merger refused |

Upload, proxy cache, singleflight and download series are format-core's and carry `format="pub"`.

## 5. Negative requirements and their tests

| Requirement | Test |
|---|---|
| A version is not published twice, and a race has one winner | `hosted::must_not_publish_a_version_twice`, `hosted::must_not_let_two_concurrent_publications_of_one_version_both_win` |
| `allow_once` still accepts a new version and still refuses a repeat | `hosted::the_write_policy_allow_once_still_publishes_new_versions` |
| A read-only write policy refuses at step 1 | `hosted::a_repository_that_takes_no_writes_refuses_the_publication_at_its_start` |
| Read-only storage is 503 and stores nothing | `hosted::read_only_mode_refuses_an_upload_with_retry_after` |
| The archive must match a declared coordinate | `hosted::must_not_publish_an_archive_whose_pubspec_differs_from_the_declared_coordinate`, `hosted::a_declared_coordinate_is_checked_against_the_archive` |
| The package inside the archive is authorized again | `hosted::the_coordinate_of_the_archive_is_authorized_again_when_the_archive_has_been_read` |
| Another writer cannot finish someone else's upload | `hosted::must_not_let_another_writer_finalize_an_upload` |
| Tar slip, links, gzip bombs, non-packages and alias bombs are not published | `hosted::must_not_publish_a_tar_slip_archive`, `hosted::must_not_publish_a_gzip_bomb`, `hosted::must_not_publish_something_that_is_not_a_package`, `hosted::must_not_expand_an_alias_bomb_in_the_manifest`, `archive::must_not_*`, `pubspec::must_not_*` |
| An oversized upload is refused before the body is read, and the stored part cannot exceed the limit | `hosted::must_not_read_the_body_of_an_upload_that_declares_too_much`, `hosted::must_not_store_more_than_the_upload_limit_whatever_the_declared_length_says` |
| One file part, one POST per upload id | `hosted::must_not_accept_an_upload_that_is_not_a_single_file_part`, `hosted::must_not_accept_a_second_upload_to_the_same_id` |
| No second method mutates a published path | `hosted::must_not_change_published_content_through_other_methods` |
| Encoded traversal does not reach storage or a different route | `hosted::must_not_let_a_path_trick_reach_storage_or_the_authorizer`, `handler::must_not_let_a_path_trick_reach_a_different_route_than_the_one_authorized` |
| 401 only for a missing token; 403 when the token is valid but the right is missing | `hosted::an_anonymous_caller_is_told_to_authenticate_on_every_route`, `hosted::must_not_answer_401_to_a_valid_token_that_lacks_the_right`, `hosted::a_failing_authorization_backend_is_403_not_401_and_never_an_allow`, `error::must_not_tell_a_caller_with_a_valid_token_to_authenticate_again`, `proxy::a_caller_without_read_rights_gets_403_from_a_proxy_too`, `group::an_anonymous_caller_of_a_group_is_told_to_authenticate`, `group::must_not_answer_401_for_a_failed_member_lookup_and_never_forward_a_member_error` |
| Advisories are not faked | `hosted::advisories_are_not_part_of_phase_1_and_are_never_faked` |
| A proxy does not publish, does not forward upstream 401 or 403, and does not cache a bad checksum | `proxy::must_not_accept_a_publication_on_a_proxy_and_must_not_read_its_body`, `proxy::must_not_forward_an_upstream_401_or_403_to_the_client`, `proxy::must_not_serve_an_archive_whose_checksum_differs_from_the_listing`, `proxy::must_not_fetch_an_archive_the_upstream_does_not_list` |
| A group does not leak a member the caller may not read, and a proxy cannot add versions of a hosted name | `group::must_not_leak_data_through_the_group_without_rights_on_the_member`, `group::must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has`, `group::the_claim_holds_even_for_a_caller_who_may_not_read_the_hosted_member` |
| A group accepts no publish | `group::nothing_can_be_published_through_a_group_and_advisories_do_not_exist_there` |
| An unreadable member listing does not become a partial success | `merge::must_not_merge_a_document_that_cannot_be_read` |
| No queue of blocking work | `blocking::must_not_queue_without_a_permit` |
| Parsers do not panic | `properties::*`, `tests/corpus.rs`, the fuzz targets (section 8) |

## 6. ADRs

1. **One handler for Dart and Flutter.** The Flutter tool runs the Dart client. A second protocol would only drift.
2. **Names and versions are canonical or refused.** Case-folding or accepting `01.0.0` would make two spellings of one coordinate, and a group claim is a string match. A version Dart would accept and this parser refuses cannot be published here. A proxy drops such an upstream entry instead of repairing it.
3. **The upload path is the authorized path.** The staged bytes live at `api/packages/versions/upload/<id>`. The published archive and the version document are written only after the manifest has been read and that coordinate has been authorized again. The client names the package inside the archive, so the first authorization cannot see it unless the client declared it.
4. **A version is create-only.** Replacing bytes under an existing version would change what a lockfile resolves. `allow` does not mean overwrite. The database unique path decides a race between two finish calls.
5. **`archive_url` is built at read time.** It depends on the repository URL the client used, including a group in front of a hosted member. Storing the upstream URL would send the client, and its token, past this server.
6. **Listings are a whitelist.** `advisoriesUpdated` is not forwarded and the advisories route is 404. An empty advisories list would claim that the package has no known vulnerabilities. Phase 1 does not have that data.
7. **401 is only a missing or invalid token.** `dart pub` deletes its stored token on 401 and keeps it on 403. `render_error` rewrites a 401 for a non-anonymous caller to 403. `hide_inaccessible_repository` is false so a hidden repository is 403, not 404.
8. **Proxy archives are fetched only when listed, and checked when a digest exists.** The deprecated archive path is what the specification tells servers to keep serving. A file that does not match `archive_sha256` is not a cacheable artifact.
9. **A hosted member pins a package name for every later proxy member.** The merger never sees the proxy document, so it cannot add versions. The claim is the component created at publish. Order is the operator's: a proxy placed first is not pinned against.
10. **An unreadable member listing fails the group.** These documents are produced by this format. Omitting one would hide versions with a 200. A member that is merely down is format-core's skip marker, which is a different failure.
11. **Archive and YAML work has its own blocking pool.** Format-core's blocking semaphore is private. The permit is acquired before the spawn. The two budgets are not shared (section 9).

## 7. Deviations and open issues

* Advisories are not implemented. The route is recognized so it is 404 rather than a generic unknown path, and so it still carries the package coordinate.
* A hosted repository cannot retract a version or mark a package discontinued. `latest` still implements those rules for documents that contain them (proxy and group).
* A crash between the archive link and the version-document write leaves an archive that is not listed. A later publish of that version is 409 until the asset is deleted. The other failure (a listing that points at a missing archive) was rejected on purpose.
* Staged uploads are reaped when a new publication starts, not on a clock. An idle repository keeps the leftovers of its last `staging_ttl`.
* Versions with leading zeros, which the Dart parser tolerates, are rejected. Proxy listings drop them.
* The ratio limit does not apply until `ratio_floor_bytes` (4 MiB) have been decompressed, so a small repetitive package is not treated as a bomb.
* `If-Modified-Since` is not implemented on documents this crate builds. They answer `If-None-Match` only.
* This crate does not run `dart pub`. End-to-end use of the `dart:stable` image belongs to the testserver harness (`docs/testserver.md`), not to these tests.
* The blocking pool does not share its budget with format-core (section 9).

## 8. Running the tests

```
cargo fmt -p rustryx-format-pub
cargo clippy -p rustryx-format-pub --all-targets --locked -- -D warnings
cargo test -p rustryx-format-pub --all-targets --locked
```

* **Unit tests** are in the modules under `src/`. They cover routes, names, versions, the archive reader, the manifest parser, documents, merge, errors, classification and the blocking pool.
* **Contract tests** (`tests/hosted.rs`, `tests/proxy.rs`, `tests/group.rs`) use the real `FormatEngine` and `PubHandler` over the testkit ports and a scripted upstream.
* **Property tests** (`tests/properties.rs`) cover names, versions, routes, archives, manifests and listings.
* **Corpus replay** (`tests/corpus.rs`) reads every file under `fuzz/corpus/<target>` and checks that the committed seeds match what the test builds. `RUSTRYX_WRITE_CORPUS=1 cargo test -p rustryx-format-pub --locked --test corpus` rewrites the seeds. There is no `RUSTRYX_IT` gate: nothing in this crate starts `dart` or Docker. Dart is not required, and a machine without it still runs the whole suite.
* **Fuzzing**: `fuzz/` is its own workspace (`rustryx-format-pub-fuzz`) with three `cargo-fuzz` targets, `pubspec`, `gzip_tar` and `listing`. They are not part of `cargo test -p rustryx-format-pub`. A nightly toolchain runs them, for example from `crates/rustryx-format-pub/fuzz`:

  ```
  cargo +nightly fuzz run gzip_tar -- -max_total_time=600
  ```

  The same entry points are what `tests/corpus.rs` calls on stable.

## 9. For format-core and the harness

Worked around, not changed:

* Format-core's blocking semaphore is crate private, so archive reads, manifest parses and listing rewrites use `BlockingPool` in this crate. A deployment can exhaust one budget while the other still has permits. A `FormatContext::run_blocking` that shared the engine semaphore would close that gap.
* The hosted pipeline audits the staging upload and the version document. The archive path is a link created here, so this crate emits `ContentEvent::AssetPublished` and `asset.publish` itself. A failed audit sink is logged and does not roll back the publish.
* `hide_inaccessible_repository` returns false. The harness note in `docs/testserver.md` matches this: a pub repository the caller may not see is 403 with the Bearer challenge, not 404. This crate already sets the challenge in `render_error`; a 401 for a non-anonymous caller is rewritten to 403 before the harness sees it.
* Proxy archive fetches rely on the format-core executor to follow redirects (maximum 3, each hop checked). This crate does not implement a redirect client of its own, and its tests use a scripted upstream that answers the archive path directly.
