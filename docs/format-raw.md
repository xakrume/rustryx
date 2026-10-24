# rustryx-format-raw

The `raw` repository format: arbitrary files under `/repository/<name>/...`. There is no public protocol for plain file repositories. The behaviour is what `crates/rustryx-format-raw` implements and what its tests lock down. One `RawHandler` implements `FormatHandler` from `rustryx-format-core` (`docs/format-core.md`). The hosted, proxy and group pipelines, ranges, conditional requests, checksum headers, the SSRF guard and the generic error envelope come from that crate. This crate decides what a path means, how a directory is listed, when a checksum side file is computed, and how content a browser would execute is delivered.

`RawHandler::formats` is only `RepoFormat::Raw`. The composition root registers one instance.

## 1. Protocol surface

| Feature | Status |
|---|---|
| `PUT <path>` stores a file on a hosted repository | supported (`201` created, `200` replaced) |
| `GET` and `HEAD <path>` read a file | supported; ranges, validators and `X-Checksum-*` come from the hosted or proxy pipeline |
| `DELETE <path>` removes a hosted file | supported (`204`, then `404`) |
| `GET` and `HEAD <dir>/` list a directory | supported; HTML by default, JSON when `Accept` prefers `application/json` |
| `<file>.sha1`, `<file>.sha256`, `<file>.md5` computed from stored digests | supported when no file of that name exists and the base file exists |
| `content_disposition` of `attachment` (default) or `inline` | supported (`format_config`) |
| Hosted, proxy and group | supported; a group does not merge, the first member that has the path wins |
| `POST`, `PATCH`, `OPTIONS`, `TRACE` on a file | not supported (`405`, `Allow: GET, HEAD, PUT, DELETE`) |
| `PUT`, `POST`, `DELETE` on a directory, including the repository root | not supported (`405`, `Allow: GET, HEAD`) |
| `PUT` and `DELETE` on a proxy or a group | not supported (`405`, `Allow: GET, HEAD`); the upstream is not called |
| A redirect from `a/b` to `a/b/` or the other way round | not supported |
| A real client | optional: `tests/real_client.rs` runs `curl` only when `RUSTRYX_IT=1` |

Paths below are the path under the repository root, percent-encoded on the wire as a client sends them.

## 2. Paths

The trailing slash is the only thing that decides. `GET a/b` addresses the file `a/b`. `GET a/b/` addresses the directory `a/b`. An empty path and `/` are the repository root, which is a directory. There is no redirect and no fallback from one to the other. A file `f.txt` makes `GET f.txt/` a missing directory (`404`). A directory that holds `dir/x.txt` makes `GET dir` a missing file (`404`). Two layers that disagree on whether `a/b` may also mean `a/b/` can authorize one thing and serve another, so the ambiguity is removed instead of repaired (SEC-19, SEC-31).

Normalization uses the strict rules of `rustryx-format-core` (`NormalizedPath` / `PathRules`) plus two rules of this format:

* depth at most 32 segments (`MAX_DEPTH`), because an upload looks up every ancestor;
* no invisible or bidirectional characters in the decoded path (zero-width characters `U+200B` through `U+200F`, bidi controls `U+202A` through `U+202E` and `U+2066` through `U+2069`, word joiners `U+2060` through `U+2064`, Arabic letter mark `U+061C`, soft hyphen `U+00AD`, byte order mark `U+FEFF`).

Shared rules that are refused, in any percent-encoding the parser understands: `..` and `.` segments, backslash, NUL and other controls, `//`, a double-encoded dot segment, and an encoded slash (`%2F`). A decoded path longer than 1024 bytes (`MAX_PATH_LEN`) or a segment longer than 255 bytes (`MAX_SEGMENT_LEN`) is refused. A segment of 255 `a` bytes is accepted. A path of exactly 32 segments is accepted. `café.txt` (UTF-8) is accepted.

A refusal from the shared parser, and a path that only fails the invisible-character rule, is `400` with the public text `invalid path`. The message does not name the rule. A path deeper than 32 segments is `400` with the public text `path is too deep`. `classify` parses the path before it looks at the method, so `GET`, `PUT` and `DELETE` of a bad path are all `400`. Authorization is not asked about a path that was refused, and a proxy or group does not forward it upstream.

A request target that a front HTTP stack rewrites before the handler sees it can be `404` instead of `400`. The HTTP test allows either status for targets that still contain `..`, an encoded `..`, a backslash, NUL, a double slash or a double encoding, and it requires that none of them answer `200`. Targets that name an existing `safe.txt` through a dot segment (`x/../safe.txt`, `./safe.txt`, `%2e/safe.txt`) are `400` and do not return the file. `curl --path-as-is` is expected to get `400` for the same family of suffixes.

Parsing does not panic. An accepted path has no `.` or `..` segment, no slash, backslash or NUL inside a segment, no control or spoofing character, and at most 32 segments. Re-encoding an accepted path (and putting the trailing slash back on a non-root directory) parses to the same target.

## 3. Hosted files

`PUT` reads the current asset, evaluates write preconditions, and only then accepts the body.

* No condition: create (`201`) or replace (`200`). The response body is empty, `Content-Length` is `0`, and `ETag` is the asset etag when it is a valid header value.
* `If-None-Match: *` on a missing path creates the file and publishes it create-only (`OverwriteRule::Never`), so a writer that lost a race is refused by the database. The same header when the file is already visible is `412`. A second request in the test sees the file and gets `412`; the stored bytes stay the first body.
* `If-Match` with the current etag, or `*`, replaces the file (`200`). A stale etag is `412` and the bytes stay. `If-Match` on a missing path is `412`.
* `If-None-Match` with the current etag is `412`.
* `DELETE` of a file is `204`. A second delete, and a get of a missing file, are `404`.

`WritePolicy::Deny` refuses `PUT` and `DELETE` with `403`. `AllowOnce` accepts the first `PUT` (`201`) and answers the second with `409`; the first bytes stay. `Allow` replaces (`200`) and the new bytes are what `GET` returns.

A path is either a file or a directory prefix, never both. Before the body of a new path is stored, the handler refuses with `409` when an ancestor is a file (`a` exists, the upload is `a/b` or `a/b/c/d`: `a parent of this path is a file, so it cannot also be a directory`) or when any asset is stored below the path (`d/x` exists, the upload is `d`: `this path is a directory, so it cannot also be a file`). Siblings are allowed. Replacing an existing file does not walk the ancestors again. Deleting the last file below a prefix frees that prefix, and it can then be uploaded as a file. The check is a read of current state, not a lock: two uploads of `a` and `a/b` that race, on one node or two, can both pass, because the asset port has no multi-path constraint. A later read still follows the trailing slash, so `GET a` is the file and `GET a/` is the listing.

`GET` of a stored file returns the bytes with `Accept-Ranges: bytes`, `X-Content-Type-Options: nosniff`, a sandboxing `Content-Security-Policy`, `ETag`, `Last-Modified`, and `X-Checksum-Sha256` (64 hex digits), `X-Checksum-Sha1` (40) and `X-Checksum-Md5`. `HEAD` has an empty body and the same `Content-Length` and `ETag`. A range `bytes=2-5` is `206` with `Content-Range: bytes 2-5/<len>` and those bytes. A suffix range `bytes=-3` is the last three bytes. A range past the end is `416` with `Content-Range: bytes */<len>`. `If-None-Match` of the current etag, and `If-Modified-Since` of the current `Last-Modified`, are `304` with an empty body. A different `If-None-Match` is `200`. A different `If-Match` on a read is `412`.

The stored content type is chosen by the shared upload pipeline, which the tests observe as follows. `readme.txt` is `text/plain`. `docs/b.md` is `text/markdown`. `pkg.tar.gz` is `application/gzip`. `index.html` uploaded as `text/html` stays `text/html`. A specific declared type such as `application/vnd.acme+json` on `x.bin` is kept. `text/html` declared for `notes.bin` is not stored as HTML: the asset is `application/octet-stream` and gets no `Content-Disposition`. A 6_000_000 byte upload round-trips with that `Content-Length`. `PUT` and `DELETE` are audited as `asset.publish` and `asset.delete`.

An offline repository (`online: false`) answers `503` before a file is served.

## 4. Directory listings

A directory exists exactly while some stored file has that prefix. `GET <dir>/` (and `HEAD`) collapses the files below the prefix into immediate children. Under `docs/`, `docs/a.md` is the file entry `a.md` and `docs/img/logo.png` is the directory entry `img`. The repository root lists the same way. Order is the bytewise order of the storage path, so files and directories interleave by name.

The engine authorizes `browse` on the directory. The listing is `ContentKind::Index` (a generated document, for the latency objective). Every file that would contribute an entry is then checked for `read` on its own path. A directory entry appears only when a readable file lies below it. A content selector that allows only part of a directory shows only that part. A directory in which nothing is readable is `404`, with the same status and body as a directory that does not exist. The root of an empty repository is `200` with an empty `items` array; any other missing directory is `404`. A caller who may `browse` but not `read` gets `200` and an empty root. A caller with no privilege on the repository gets `404` for the listing. Anonymous gets `401`.

On the wire, browse of a directory is authorized on the path without the trailing slash. A content selector that should allow listing `/public/` has to name both `path == "/public"` and `path.startsWith("/public/")`. A directory the selector does not allow at all is `403`, not an empty `200`.

Paging. `limit` defaults to 200 (`DEFAULT_LIMIT`) and must be an integer from 1 to 1000 (`MAX_LIMIT`). `0`, `1001`, `-1`, `abc` and an empty `limit` are `400` (`invalid limit`). One page is made of whole storage pages of `limit` files. The continuation token carries the asset-port cursor (the position after the last file that was looked at) and the last directory that was emitted, so a directory that spans two storage pages is not listed twice. A full page therefore holds at least `limit` entries unless the directory ends, and fewer than `2 * limit`. A page can also be shorter, or empty with a token, when unreadable files had to be skipped.

One request looks at no more than `MAX_SCANNED` files (5000), or the limit passed to `RawHandler::with_scan_limit`. A bound of 0 is raised to 1, because a zero bound would make every listing empty. When the bound is hit the response returns what it has and a token. Following the token continues the scan. The handler does not ask the authorizer once per remaining file after a `read` lookup fails: the listing is `403`, the provider's error text is not in the body, and exactly one `read` check was made.

The token names the directory it was issued for (`p` is the storage prefix, empty for the root, `a/b/` otherwise). Using it on another directory is `400` (`invalid token`). A token longer than 2048 characters, a token that is not URL-safe base64 without padding of a JSON object of that shape, and the payloads `!!!` and `e30` are the same `400`. The token is not a secret and grants nothing: the same `read` checks run on every page.

`Accept` weighs only the two representations this format has. Media ranges are read in order with their `q` value. The higher best `q` wins. A tie goes to the range listed first. `application/json` (parameters ignored, case ignored) selects JSON. `text/html`, `application/xhtml+xml`, `text/*` and `*/*` select HTML. A missing `Accept`, an empty value, `application/xml`, and a non-text header select HTML. `q=0` drops a representation. A weight that does not parse counts as 1. A weight outside 0 to 1 is clamped. Negotiation does not panic on those inputs.

JSON (`Content-Type: application/json`):

```json
{
  "repository": "files",
  "path": "docs/",
  "items": [
    {
      "name": "a.md",
      "type": "file",
      "size": 2,
      "lastModified": "2026-10-24T10:00:00Z",
      "contentType": "text/markdown",
      "sha256": "<64 lowercase hex digits>"
    },
    { "name": "img", "type": "directory" }
  ],
  "continuationToken": null
}
```

`path` has no leading slash and a trailing slash, and is empty at the root. `lastModified` is RFC 3339. A directory item has no `size`. `continuationToken` is JSON `null` when there is no next page. `repository` is the repository the client addressed, including when that repository is a group. The listing does not include computed side files, only stored assets.

HTML (`Content-Type: text/html; charset=utf-8`) is a document titled `Index of /repository/<repository>/<path>`. The root has no parent link. Any other directory links `../`. Each name is a relative link `./<percent-encoded segment>` with the visible text HTML-escaped (`&`, `<`, `>`, `"`, `'`). A file size is shown in `B`, `KiB`, `MiB`, `GiB` or `TiB` (one decimal above 1023 bytes) and the exact byte count is the `title` of that cell (`1536 bytes`, displayed `1.5 KiB`). `lastModified` is escaped. A next page is `?limit=<n>&token=<escaped token>`. Names are percent-encoded so that only unreserved characters stay literal, which means a file called `javascript:alert(1)` cannot start a scheme URL and a name cannot close the attribute. `<script>` and `<img` from a file name do not appear raw in the HTML.

Both representations send `X-Content-Type-Options: nosniff`, `Cache-Control: no-cache`, `Vary: Accept`, and `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`. The page has no script source. `HEAD` has an empty body and a `Content-Length`.

## 5. Checksum side files

`<file>.sha1`, `<file>.sha256` and `<file>.md5` are answered from the digests the blob store stored with the base file. Nothing is written for them. The body is the bare lowercase hex digest with no newline, the same text as the matching `X-Checksum-*` header of the base file (40, 64 and 32 digits). `Content-Type` is `text/plain; charset=utf-8`. The response also carries `ETag`, `Last-Modified` of the base file, `Content-Length`, `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox; default-src 'none'`. The etag is `"<base sha256>-sha1"`, `"<base sha256>-sha256"` or `"<base sha256>-md5"`. `HEAD` is empty with that length. `If-None-Match` of that etag is `304`.

A name that is only the suffix (`.sha1`, `a/.sha1`) is an ordinary file name, not a side file. `a/sha1`, `a/b.sha512` and `a/b.sha1x` are not side files. `a.sha1.sha256` peels one suffix: the base name is `a.sha1` and the digest is SHA-256. A file uploaded under the side file's own name always wins (`app.tgz.sha256` can be `deadbeef  app.tgz\n`). A missing base file has no computed side file (`404`), and the computed name is not a listing entry.

The caller needs `read` on the base file as well as on the side path. A prefix the caller may not read is `403` for both the file and `file.sha256`, while a sibling they may read still returns the digest. When the engine allows the side path and denies only the base path, the side file is `404` for that caller, not the digest. A group answers the side file from the member that holds the base. A proxy computes a side file only from a base file already in the cache. It does not fetch the upstream object just to hash it: before the base is cached the side file is `404` and the upstream is not called; after a cached `GET` of the base, the side file is the SHA-256 of those bytes.

## 6. Active content

`Content-Disposition` is a decision of this format, applied to the response of a file read on a hosted repository, a proxy and a group (including a file served through a group member). The shared pipeline already sets `nosniff` and a sandboxing `Content-Security-Policy`. This step adds what is missing.

Every successful response gets `X-Content-Type-Options: nosniff` if the header is absent. A `200` or `206` whose content type a browser would execute gets `Content-Disposition: attachment; filename="<ascii>"; filename*=UTF-8''<percent-encoded>` unless the repository opts into `inline`, or the response already has a `Content-Disposition` (that value is kept, even `inline`). `304`, `404`, `416` and `500` are left unchanged. Inert types (`text/plain`, `application/json`, `application/octet-stream`, `application/gzip`, `image/png`, `application/pdf`, `text/css`, an empty type) get nosniff and no disposition.

Active types, compared on the media type without parameters and in any case: `text/html`, `application/xhtml+xml`, `image/svg+xml`, `text/xml`, `application/xml`, any type ending in `+xml`, `text/javascript`, `application/javascript`, `application/x-javascript`, `text/ecmascript`, `application/ecmascript`, `text/xsl`, `application/xslt+xml`, `text/cache-manifest`. The hosted tests lock attachment behaviour for `text/html` (including a name `page.htm` and a byte range), `image/svg+xml`, `application/xml` and `text/javascript`. The same attachment is applied to HTML fetched by a proxy (on a cache miss and on a hit) and to HTML read through a group.

The filename fallback keeps only ASCII letters, digits and `. - _ space + ( )`. Anything else, including quotes, semicolons and CR/LF, becomes `_`, so a hostile name cannot break out of the quoted parameter. The `filename*` parameter percent-encodes every byte that is not an ASCII letter, digit or `. - _ +`. A non-ASCII character such as `é` is `%C3%A9`. When the path has no file name the fallback is `download`.

`inline` omits `Content-Disposition` for active types and still sends `nosniff` and the pipeline CSP, so a script in the file is not given the origin of the artifact manager. Any other `content_disposition` value, a non-string, `null`, an array, or a `format_config` that is not an object, means `attachment`. The word `inline` is accepted with surrounding spaces and in any ASCII case.

## 7. Proxy

A proxy caches by path. The first `GET` is `X-Rustryx-Cache: MISS` and the body is the upstream body. After the copy is stored, the next `GET` is `HIT` and the upstream is not called again. Ranges are answered from the cache. When the upstream sends `X-Checksum-*`, those headers are the expected digests and the body is verified before a byte reaches the client. A wrong `X-Checksum-Sha256` is `502`, nothing is cached, and the next `GET` calls the upstream again. A matching digest is served. An upstream that does not send those headers is streamed while it is cached (the fetch-once test uses a body with no checksum header).

An upstream `401` or `403` is `502` with no `WWW-Authenticate`. A missing upstream file is `404`, and a second lookup inside the negative-cache window does not call the upstream again. A cached copy past its freshness is revalidated with the upstream validators (`If-None-Match` of the upstream etag). When that revalidation is `304`, the client gets `200` and `X-Rustryx-Cache: HIT` with the cached bytes. When the upstream then fails (`500`) the cached bytes are served as `X-Rustryx-Cache: STALE` with a `Warning` that starts with `110`.

`PUT` and `DELETE` are `405` with `Allow: GET, HEAD` and do not call the upstream. A listing is the files already cached under that prefix. Before anything is cached the directory is `404`. The listing never requests the directory path from the upstream.

## 8. Group

A group answers a file with the first member that has the path. There is no merger. A hosted member ordered before a proxy answers the file and the proxy's upstream is not called; a path only the proxy has is fetched from that upstream. `HEAD` works the same way. A path no member has is `404`.

A listing is the listing of the first member that has that directory, not a union. The JSON `repository` field is the group name. A directory only a later member has is listed from that member. A directory nobody has is `404`.

`PUT` and `DELETE`, of a file or of a directory, are `405` with `Allow: GET, HEAD`.

A caller who may read the group and one member, but not an earlier member, does not receive that earlier member's bytes: a path both members have is served from the member they may read, and a path only the closed member has is `404`. A caller who may read the group and no member gets `404`. Anonymous gets `401`. HTML read through the group is still an attachment. Checksum side files are answered from the member that holds the base file.

## 9. Authorization and errors

Anonymous is `401` with `WWW-Authenticate: Basic` unless both the server anonymous switch and the repository `allow_anonymous` flag are on. A grant to the anonymous principal does not bypass a repository that has the flag off. Anonymous `PUT` is `401` even on a repository that allows anonymous reads. Turning the server switch off makes the same read `401` again. Wrong credentials are `401`. A reader may `GET` and may not `PUT` or `DELETE` (`403`); the stored bytes stay. A principal who may not see the repository gets `404` for a read and does not learn that the file exists. An API token is accepted as the Basic password and as a bearer token by the HTTP harness; that is the server's authentication, not a raw-format header.

A failing authorizer fails closed: `GET` and `PUT` are `403`, and a `PUT` that was refused stores nothing. A listing whose `browse` check fails is `403`. `AuthError::Overloaded` from a `read` check inside a listing or a side-file read is `503` with `Retry-After: 1` and the public reason `authorization capacity exhausted, retry`. Every other authorization error is `403`. `map_auth_error` does not log; the engine logs the final error once.

Client errors use the generic JSON envelope (`Content-Type: application/json`, `error.code`, `error.message`). A missing file is `not_found`. A bad path message contains `invalid path` and does not contain the word `traversal`. Conflict text from section 3 is the public message of a `409`.

## 10. Configuration

`Repository::format_config` for this format:

```json
{ "content_disposition": "attachment" }
```

The only key read by this crate is `content_disposition` (`CONTENT_DISPOSITION_KEY`). `attachment` is the default. `inline` is the only opt-in. See section 6.

## 11. Limits

| Limit | Value |
|---|---|
| `MAX_DEPTH` | 32 segments |
| `MAX_PATH_LEN` | 1024 decoded bytes |
| `MAX_SEGMENT_LEN` | 255 decoded bytes |
| `DEFAULT_LIMIT` | 200 files per storage page |
| `MAX_LIMIT` | 1000 |
| `MAX_SCANNED` | 5000 files looked at per listing request |
| continuation token | 2048 characters |
| `with_scan_limit(0)` | clamped to 1 |

This crate defines no metrics of its own.

## 12. Negative requirements and their tests

| Requirement | Test |
|---|---|
| No traversal, backslash, NUL, control, double slash, encoded slash, double encoding or dot segment, and authorization never sees the refused path | `target::tests::must_not_accept_traversal_in_any_form`, `target::tests::must_not_accept_backslash_nul_control_and_double_slash`, `handler::tests::a_bad_path_is_a_bad_request_before_the_method_is_looked_at`, `hosted::must_not_resolve_traversal_in_any_form_and_authorization_never_sees_it`, `http::must_not_resolve_a_traversal_target_as_written_by_the_client`, `proxy_group::must_not_resolve_traversal_through_a_proxy_or_a_group`, `real_client::curl_gets_401_without_credentials_and_cannot_use_a_traversal` |
| A refusal from the shared parser does not name which rule it was | `target::tests::a_failure_does_not_say_which_rule_it_was`, `hosted::errors_use_the_generic_envelope_without_internal_text` |
| Over-long segments, over-long paths and paths deeper than 32 segments are refused | `target::tests::must_not_accept_overlong_paths_and_segments`, `hosted::refuses_overlong_and_too_deep_paths` |
| Invisible and bidirectional characters are refused; an accepted path never contains them | `target::tests::must_not_accept_invisible_and_bidirectional_characters`, `target::tests::parsing_never_panics_and_never_yields_a_traversal` |
| A path is not both a file and a directory, and the trailing slash never converts one into the other | `hosted::a_path_is_a_file_or_a_directory_never_both`, `hosted::the_trailing_slash_never_turns_a_file_into_a_directory`, `real_client::curl_uploads_a_chunked_body_and_meets_the_file_and_directory_rule` |
| A directory is not written or deleted | `handler::tests::directories_are_listed_and_never_written`, `hosted::the_trailing_slash_never_turns_a_file_into_a_directory` |
| Stored HTML, SVG, XML and script are not served inline by default, including ranges, proxies and groups | `disposition::tests::must_not_serve_stored_html_inline_by_default`, `hosted::must_not_serve_stored_html_inline`, `http::html_uploads_come_back_as_attachments_over_http`, `proxy_group::must_not_serve_upstream_html_inline`, `proxy_group::must_not_serve_html_inline_through_a_group`, `real_client::curl_sees_html_as_an_attachment` |
| A hostile file name cannot break out of `Content-Disposition` | `disposition::tests::a_hostile_file_name_cannot_break_out_of_the_header` |
| A file name cannot inject markup or a `javascript:` link into a listing | `listing::tests::must_not_let_a_file_name_inject_markup_or_a_script_link`, `listing::must_not_let_a_file_name_turn_the_listing_into_markup_or_a_script_link` |
| A listing does not show what the caller may not read, and a directory with nothing readable is not distinguishable from a missing one | `listing::must_not_list_what_the_caller_may_not_read`, `listing::a_directory_whose_files_are_all_closed_looks_like_a_missing_one`, `http::a_content_selector_limits_both_downloads_and_the_listing` |
| One listing request does not scan past its bound, and a failed `read` check does not continue through the remaining files | `listing::must_not_scan_more_than_the_bound_for_one_request`, `listing::a_failing_read_check_stops_instead_of_walking_every_file` |
| A side file does not reveal the digest of a base file the caller may not read, and a closed base is not hashed for a caller who can name the side path | `hosted::must_not_reveal_the_digest_of_a_file_the_caller_may_not_read`, `hosted::must_not_hand_out_a_side_file_when_the_base_file_is_closed` |
| A proxy does not fetch a base file only to compute a side file, does not cache a bad upstream checksum, does not forward an upstream `401` or `403`, and does not ask the upstream for a directory | `proxy_group::a_proxy_computes_a_side_file_only_from_a_cached_base`, `proxy_group::must_not_serve_or_cache_content_with_a_wrong_upstream_checksum`, `proxy_group::must_not_forward_an_upstream_401_or_403`, `proxy_group::a_proxy_lists_what_it_has_cached` |
| A group does not merge, does not accept writes, and does not leak a member the caller may not read | `proxy_group::a_group_answers_with_the_first_member_that_has_the_path`, `proxy_group::a_group_lists_the_first_member_that_has_the_directory`, `proxy_group::a_group_takes_no_writes`, `proxy_group::must_not_leak_a_members_file_through_a_group_without_the_members_right`, `http::a_group_over_http_serves_the_first_member_and_refuses_writes` |
| Anonymous is not allowed without the repository flag, and anonymous does not write | `hosted::must_not_allow_anonymous_access_without_the_repository_flag`, `http::anonymous_read_needs_the_switch_and_the_repository_flag` |
| `Accept` negotiation does not panic on a bad weight or a non-text value | `accept::tests::malformed_weights_and_values_do_not_panic` |

`cargo test -p rustryx-format-raw` does not run the fuzz targets. `crates/rustryx-format-raw/fuzz` is its own workspace (`rustryx-format-raw-fuzz`) and needs a nightly toolchain. `path_normalizer` checks the same path invariants as the proptest. `accept_header` checks that arbitrary `Accept` bytes do not panic.

## 13. Known limit

The file-or-directory check is not atomic across paths. Concurrent uploads of a path and of a path below it can both succeed. Reads still do not treat one as the other.

`AuthError::Overloaded` is mapped to `503` in `map_auth_error`. No test in this crate drives that variant; the tests drive `GroupLookupFailed`, which is `403`.

## 14. Running the tests

```
cargo fmt -p rustryx-format-raw
cargo clippy -p rustryx-format-raw --all-targets --locked -- -D warnings
cargo test -p rustryx-format-raw --all-targets --locked
```

* Unit tests live in `src/accept.rs`, `src/config.rs`, `src/disposition.rs`, `src/handler.rs`, `src/listing.rs`, `src/sidecar.rs`, `src/target.rs` and `src/write.rs`. `target` includes `proptest` cases.
* `tests/hosted.rs`, `tests/listing.rs` and `tests/proxy_group.rs` run `RawHandler` through the real format engine and the in-memory ports (`tests/support/mod.rs`).
* `tests/http.rs` uses the HTTP harness (`tests/common/mod.rs`).
* `tests/real_client.rs` skips, and prints `SKIP`, unless `RUSTRYX_IT=1` and `curl` runs. Do not set `RUSTRYX_IT` for the default run. The five tests then pass without starting `curl`.
