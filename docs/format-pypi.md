# rustryx-format-pypi

The PyPI repository format (`RepoFormat::Pypi`): hosted, proxy and group repositories that speak
the simple repository API and the legacy upload API. One `PypiHandler` implements `FormatHandler`
of `rustryx-format-core` (`docs/format-core.md`); the hosted, proxy and group pipelines, the SSRF
guard, the resilience stack and the security invariants of those pipelines come from there. This
crate is the protocol.

Public protocol sources (no code or text of any other artifact manager was used):

* simple repository API (PEP 503, PEP 691): <https://packaging.python.org/en/latest/specifications/simple-repository-api/>
* name normalization: <https://packaging.python.org/en/latest/specifications/name-normalization/>
* version specifiers (PEP 440): <https://packaging.python.org/en/latest/specifications/version-specifiers/>
* binary distribution file names (PEP 427): <https://packaging.python.org/en/latest/specifications/binary-distribution-format/>
* source distribution file names: <https://packaging.python.org/en/latest/specifications/source-distribution-format/>
* core metadata: <https://packaging.python.org/en/latest/specifications/core-metadata/>
* yank (PEP 592), metadata files (PEP 658, PEP 714), `upload-time` (PEP 700), as used by the
  simple API page linked above
* legacy upload API: <https://docs.pypi.org/api/upload/>
* `multipart/form-data`: RFC 7578 and RFC 2046 section 5.1

Contents: 1 protocol surface, 2 behaviour, 3 configuration, 4 limits and metrics, 5 negative
requirements and their tests, 6 ADRs, 7 deviations and open issues, 8 running the tests, 9 what
format-core and the harness should know.

## 1. Protocol surface

| Feature | Status |
|---|---|
| `GET` and `HEAD` `/simple/` and `/simple/<project>/` (PEP 503 HTML, PEP 691 JSON and HTML) | supported (section 2.1, 2.5) |
| Content negotiation: `application/vnd.pypi.simple.v1+json`, `application/vnd.pypi.simple.v1+html`, their `latest` aliases, `text/html`; missing `Accept` or `*/*` is HTML; anything else is 406 | supported |
| Project page attributes: file URL with a hash fragment, `requires-python`, yank (PEP 592), `core-metadata` and `dist-info-metadata` (PEP 658, PEP 714), `size` and `upload-time` (PEP 700) | supported on generated and parsed pages |
| `GET` and `HEAD` `/packages/<project>/<file>` for `.whl`, `.tar.gz` and `.zip` | supported |
| `GET` and `HEAD` `/packages/<project>/<file>.metadata` | supported (hosted: the stored sidecar; proxy: fetched when the index lists it) |
| `GET` `/pypi/<project>/json` and `GET` `/pypi/<project>/<version>/json` | supported, derived from the same page as the simple index (section 2.5) |
| `POST /` legacy upload (`:action=file_upload`, multipart, file part named `content`) | supported on hosted (section 2.2) |
| Declared `sha256_digest`, `md5_digest` (hex or URL-safe base64 without padding), `blake2_256_digest` | supported: verified when present; omitting them is accepted |
| Wheel `METADATA` checked against the file name, stored as the PEP 658 sidecar | supported |
| `PUT` and `DELETE` `/packages/<project>/<file>.yanked` (body or `?reason=`) | supported on hosted; a second yank does not overwrite the marker |
| `DELETE` `/packages/<project>/<file>` | supported on hosted (file, then it is gone; sidecars go first) |
| Proxy of an upstream simple index, URLs rewritten to this repository, file hash checked | supported when the file URL is under `remote_url` (section 2.3) |
| Group merge of project and root indexes, dependency confusion claim | supported (section 2.4) |
| `gpg_signature`, `description`, classifiers and any other form field | dropped, not stored |
| `.egg`, `.tar.bz2`, `.exe` and other file types | not accepted (404 on read, 400 on upload) |
| Files hosted on a different origin than `remote_url` (the Warehouse layout, files on `files.pythonhosted.org`) | not fetched (502, section 7) |
| Repository `format_config` | not read |

## 2. Behaviour

### 2.1 Classification (before the body is read)

`classify` normalizes the path once (`NormalizedPath`, trailing slash allowed, traversal in any
encoding is a 400) and rebuilds the path from the parsed route. The project name in that path is
the PEP 503 form (lowercase, runs of `-`, `_` and `.` collapsed to one `-`). `handle` reads only
that path. `/simple/Foo_Bar/` is authorized, routed and cached as `simple/foo-bar`.

| Request | Action | Kind |
|---|---|---|
| `POST /` | write | not an index |
| `GET` or `HEAD` `/simple/` | browse | index |
| `GET` or `HEAD` `/simple/<project>/` | read | index |
| `GET` or `HEAD` `/packages/<project>/<file>` | read | artifact |
| `DELETE` `/packages/<project>/<file>` | delete | artifact |
| `GET` or `HEAD` `/packages/<project>/<file>.metadata` | read | not an index |
| `GET` or `HEAD` `/packages/<project>/<file>.yanked` | read | not an index |
| `PUT` or `DELETE` of a yank marker | write | not an index |
| `GET` or `HEAD` `/pypi/<project>/json` and `/pypi/<project>/<version>/json` | read | index |

Any other method on a known route is 405 with `Allow` set to the methods of that route (`POST`
on `/`; `GET, HEAD` on indexes, metadata and the JSON API; `GET, HEAD, DELETE` on a file;
`GET, HEAD, PUT, DELETE` on a yank marker). A path that is not one of these routes is 404,
including a project name that fails validation (`simple/-bad-`), a file that is not a wheel or
an sdist, a file whose name names a different project than the directory, and a version in the
JSON API that is not PEP 440. `GET /` is 404.

The coordinate content selectors see is `(name, version, extension)` for a file, its metadata
and its yank marker (`whl`, `tar.gz` or `zip`); name only for a project page; name and version
for a version JSON document. The root index and `POST /` have no coordinate. The version is the
canonical PEP 440 text.

A project name is at most 128 bytes, ASCII letters, digits, `-`, `_` and `.`, and must start
and end with a letter or digit. A version is at most 128 bytes. A file name is at most 255
bytes, ASCII letters, digits, `.`, `_`, `-` and `+` only. A wheel is
`{name}-{version}[-{build}]-{python}-{abi}-{platform}.whl` (5 or 6 dash separated parts; a build
tag starts with a digit; tags are non-empty and contain only letters, digits, `.` and `_`).
An sdist is `{name}-{version}.tar.gz` or `.zip`, and the version is the text after the last
dash. The name inside a file is normalized with the same rules, so `Foo_Bar-1.0.0-py3-none-any.whl`
is the project `foo-bar`.

PEP 440 parsing accepts surrounding ASCII whitespace, a leading `v` and any case, and the
spellings the tests pin (`1.1RC1` is `1.1rc1`, `1.2.alpha.3` is `1.2a3`, `1.0-1` is `1.0.post1`,
`1.0+UBUNTU_01` is `1.0+ubuntu.1`, epoch `0` is omitted). A separator with nothing after it is
refused (`1.0a-`, `1.`). Comparison follows the PEP 440 example order in
`version::orders_like_the_specification_example`. Trailing zeros compare equal (`1`, `1.0` and
`1.0.0`) but the canonical text keeps the zeros that were written (`1.0.0` stays `1.0.0`,
`00.09000` is `0.9000`). A number that does not fit in 64 bits, more than 32 release segments
and more than 32 local segments are refused.

### 2.2 Hosted

Upload order, so nothing is stored before it has been checked and the file is not held in memory:

1. Form fields before the file are read. Retained fields are `:action`, `protocol_version`,
   `name`, `version`, `filetype`, `pyversion`, `metadata_version`, `requires_python`,
   `md5_digest`, `sha256_digest` and `blake2_256_digest`. Each value is at most 4096 bytes.
   Every other field, including `description` and a `gpg_signature` part, is counted against a
   16 MiB budget and dropped. A retained field twice is a 400. The part named `content` must
   carry a file name and must come after the fields (clients send it last). A file part first
   leaves the required fields missing.
2. `:action` must be `file_upload`. `protocol_version`, when present, must be `1`. `name` and
   `version` are parsed. `filetype` is `bdist_wheel` or `sdist`. `requires_python` is at most
   256 bytes of printable ASCII (or empty, which means absent). `metadata_version` is at most
   16 characters of digits and dots. Digests are optional; a present digest must decode.
3. The file name must parse and must agree with `name`, `version` and `filetype` after
   normalization (`Foo_Bar` with `foo-bar` agrees; a different project or version does not).
4. Authorization runs again with that coordinate. The engine could only see `POST /`, which has
   no project. A content selector on the project name is applied here, before a byte of the file
   is read. Allow continues. Deny is 403. An overloaded authorizer is 503 with `Retry-After: 1`.
   Any other authorizer error is 403.
5. An existing path is 409 (`File already exists: <file>. A file name can be used once; upload a new version instead.`) before the file bytes are read. The upload is create-only (`OverwriteRule::Never`). A conflict the pipeline reports is the same 409.
6. The file part streams into the hosted pipeline. `md5_digest` and `blake2_256_digest` are
   hashed on that stream in slices of at most 64 KiB. A mismatch fails the stream at its end,
   which aborts the staged blob, and the client gets 400 `DigestMismatch`. `sha256_digest` is
   passed to the pipeline as the expected checksum and checked there the same way. A body that
   ends before the closing delimiter fails the same way (400, or 408 when the client stalls
   past `Limits::body_idle_timeout`).
7. For a wheel, `METADATA` is read with ranged reads (stored or deflate, ZIP64 end records
   included). Name and version must agree with the file name. `Requires-Python` in the file
   wins over the form. The metadata becomes `<file>.metadata`. Every upload then writes
   `<file>.info.json` (`requires_python`, `metadata_version`, record version 1). Stale
   `.yanked`, `.info.json` and `.metadata` at that file name are deleted first. A failure in
   this step deletes the file and those records again. Success is 200 with an empty body and
   `Content-Length: 0`.

Known limit, stated in the upload module: between the commit of the file and the compensating
delete the file can be downloaded. If the finalizer task itself panics, that delete is not run.

A project page is one paged listing of `packages/<project>/` (1000 assets a page) plus the
sidecars. A path that is not `<file>`, `<file>.metadata`, `<file>.info.json` or `<file>.yanked`,
or whose file name does not belong to the project, is ignored. Each file is listed with its
stored SHA-256, size, `upload-time` (`2024-05-01T10:00:00.123456Z`, UTC), `requires-python` from
`.info.json` when that record parses, `core-metadata` hashes from the metadata asset's SHA-256
when the sidecar exists, and the yank reason when `.yanked` exists. No component of that name is
404. A component whose files were all deleted still produces an empty page, so the name stays
claimed. More files than `max_index_files` (or more assets than four times that) is 502, not a
short page.

The root index is the distinct component names that parse as project names. The scan stops at
`max_root_projects` names or `max_root_scan_components` components. When the listing still had
another page, or the name list was longer than the cap, the response is marked incomplete
(`x-rustryx-index-truncated: true`, `Cache-Control: no-store`).

Yank, hosted only (a proxy or group GET of `.yanked` is 404 from this handler; PUT and DELETE
never reach it, see section 2.4):

* The file must exist, or the answer is 404.
* `PUT` when a marker already exists returns 200 and the stored reason. The new body is not
  applied. Markers are created, not rewritten (`OverwriteRule::Never`), which works under
  `allow_once`.
* `PUT` when no marker exists reads the body (or, when the body is empty or whitespace,
  `?reason=`). The reason is trimmed, at most 1024 bytes, and must not contain a control
  character. The marker is `{"reason":"..."}` with content type `application/json`. The answer
  is 201 `{"yanked":true,"reason":"..."}`.
* `DELETE` removes the marker (204) or 404 when there is none.
* `GET` returns the same JSON, or 404 when the file is not yanked.
* A marker that is not valid JSON, or whose reason fails validation (including a control
  character), still means yanked, with an empty reason. A sidecar over 64 KiB is treated as empty.

`DELETE` of a file removes `.yanked`, `.info.json` and `.metadata` first, then the file (204).
A missing file is 404. A failure leaves a file that is still listed.

`GET` and `HEAD` of a file and of `.metadata` are the hosted download pipeline (conditional
requests, ranges and checksum headers from format-core).

### 2.3 Proxy

The project page and the root index are upstream documents, cached by the proxy pipeline for
the repository metadata TTL. The request asks for JSON, then HTML
(`application/vnd.pypi.simple.v1+json`, then `v1+html`, then `text/html`). JSON is recognized
by a content type that contains `json`, or by a body whose first non-space byte is `{`.
Relative links are resolved against the upstream page URL (and `<base href>` on top of that).
Targets that are not `http` or `https` are dropped.

The page the client sees uses this repository's URLs
(`<origin>/packages/<normalized>/<filename>`), keeps only file names that parse as a
distribution of that project, and keeps the first copy of a duplicated file name. The name in
the document is the name of the request, never a name read from the document. Hashes, yank,
`requires-python`, core metadata, size and upload time are kept. An upstream file whose URL is
not on the same scheme, host and port as `remote_url`, or not below its path, or which carries
a username or password, is not fetched: the client gets 502 (`upstream_blocked`) and
`pypi_proxy_entry_unreachable_total` increments. That is why an index that points at
`files.pythonhosted.org` while `remote_url` is `https://pypi.org/simple` cannot be mirrored.

A file is served only when that page lists its exact file name. The fetch is verified against
the hash the index lists, SHA-256, else SHA-1, else MD5. SHA-512 alone produces no expectation
(the pipeline has no SHA-512 checksum). A cached copy whose digest is not the hash now listed
is deleted and counted (`pypi_proxy_cache_evicted_total`) before the fill. No listed hash means
the fill is unverified.

`.metadata` is fetched only when the index entry has core metadata. The URL is the file URL
plus `.metadata`, still under `remote_url`. When the index gives hashes for that metadata,
the same preference (SHA-256, SHA-1, MD5) is the expectation. `data-core-metadata="true"` with
no hash is fetched unverified. No core metadata attribute is 404.

The root index is truncated to `max_root_projects` with the same incomplete marker as hosted.
A project page over `max_index_files` is 502. A document that does not parse is 502. More
entries than the parser allows is 502 (`IndexTooLarge`).

### 2.4 Group

Index routes are merged. The handler asks members for the simple page as JSON (conditional
headers and `Range` stripped), whatever the client asked for. A member document whose content
type contains `json` is parsed as PEP 691; otherwise as HTML. A document that does not parse
is skipped and counted only in the log. If none remain, the answer is 422. Duplicate file
names: the first member in group order wins. The merged project name is the request's
normalized name. Files are sorted by PEP 440 version, then by file name. Root names are
normalized, sorted and deduplicated.

Dependency confusion:

* `group_claims` returns `NameClaim::package` of the coordinate name. The root index and an
  upload have no claim. The group pipeline of format-core applies the claim against the
  component table, including for a caller who cannot read the claiming member, and fails closed.
* The merger also drops every non-hosted member document when any document `holds_claim`, even
  if that proxy is listed before the hosted member. Page merging does not depend on member
  order.
* A direct file URL does depend on order: list hosted members before proxy members. The claim
  stops later proxy members from being asked; it does not hide a proxy that sits earlier and
  already has the path.

More than 50 000 files in a merged project page, or more than 1 000 000 project names
accumulated across root documents before deduplication, is 502. Those caps are the parser
constants, not `PypiConfig`.

The JSON API of a group is rendered from that merged simple page.

File, metadata and yank routes on a group are `group().resolve` (yank GET then returns 404,
because `serve_yank` allows hosted only). Writes, including upload, yank, unyank and delete,
are 405 with `Allow: GET, HEAD` from the engine, before the body is read and before this
handler runs. The same 405 applies to a proxy.

### 2.5 Pages, JSON API, errors

Generated and rewritten index documents carry a strong `ETag` (quoted SHA-256 of the bytes
sent), `Content-Type`, `Content-Length`, `X-Content-Type-Options: nosniff` and
`Cache-Control: no-cache` (or `no-store` when the root index is known to be incomplete).
`If-None-Match` on those bytes is 304 with no body. `If-Modified-Since` is not evaluated.
`HEAD` has no body and still has `Content-Length`. A representation chosen from `Accept`
varies on `Accept`. A proxied page also keeps `x-rustryx-cache`, `Warning` and
`Last-Modified` from the cached upstream document. A rendered body larger than
`Limits::max_document_bytes` is 502.

Negotiation: no `Accept`, or an empty one, is `text/html`. Quality and specificity pick among
the types in section 1. `application/*` can select JSON, `text/*` selects `text/html`. A
request that accepts none of them is 406, plain text, `Vary: Accept`. That 406 is not a
`FormatError`: it has no `x-request-id`.

HTML is a tolerant tokenizer (anchors, comments, quoted and unquoted attributes, the entities
used in names and URLs, `<base href>`). It is not an XML parser. Rendering escapes `&`, `"`,
`<` and `>` in attributes and `&`, `<` and `>` in text. A yank with an empty reason still
emits `data-yanked=""`. JSON rendering uses API version `1.1` and a `versions` array only when
every file has a size; otherwise `1.0` without `versions`. `core-metadata` is repeated as
`dist-info-metadata`. An empty yank reason is JSON `true`; a reason is a string. Unknown JSON
keys are ignored. A file without `filename` or `url` is skipped. A hash that is not hex is
dropped. `yanked: false` and `null` mean not yanked; `true`, a string, an object or a number
mean yanked (a hostile value is yanked, not ignored).

The JSON API is `application/json` with no `Vary`. `info` has `name`, `version`,
`requires_python`, `yanked` and `yanked_reason`. `version` is the highest release that is not a
pre-release and not fully yanked, else the highest that still has a file that is not yanked,
else the highest. `urls` describes that release. Without a version in the path, `releases` maps
every canonical version to its files. `last_serial` is 0, `has_sig` is false, `comment_text` is
empty and `vulnerabilities` is empty. Summary, classifiers and license are not present: the
page does not know them. A requested version with no file is 404. Pre-release detection is PEP
440 (`a`, `b`, `rc`, `dev`).

Other errors are plain text (`text/plain; charset=utf-8`, the client safe message plus a
newline, `nosniff`). A 401 carries `WWW-Authenticate: Basic realm="Rustryx", charset="UTF-8"`.
`Retry-After`, `Allow` and the other headers of the error are copied. `x-request-id` is set
when the id is a valid header value. `IndexTooLarge` is 502 and the body is
`the package index is too large to serve`, not the generic message. Status mapping is
format-core's: conflict 409, digest mismatch 400, anonymous 401, no right 403, bad path 400,
unreadable merged index 422, oversized form field budget 413, stall 408.

An anonymous `POST` to a hosted repository whose `allow_anonymous` flag is off is 401 from the
engine, before this handler reads the body, even when the authorizer would allow the call.
That is `safety::must_not_accept_an_anonymous_upload_without_the_repository_flag`. This crate
does not itself reject an anonymous principal inside `handle`. With `allow_anonymous` set, the
engine's authorizer and the second check in step 4 of the upload both see the caller; a checker
that allows the write will publish the file.

## 3. Configuration

This crate does not read `format_config`. Write policy, `allow_anonymous`, proxy `remote_url`
and metadata TTL are the repository fields format-core already applies. There is no version
policy: a hosted repository accepts any PEP 440 version the file name carries.

Per handler (`PypiConfig`, defaults in parentheses):

| Field | Default | Effect |
|---|---|---|
| `max_index_files` | 50 000 | Hosted and proxy project pages over this are 502. The group merger uses the same number as a constant, not this field. |
| `max_root_projects` | 100 000 | Hosted and proxy root indexes are cut here. The group root uses 1 000 000. |
| `max_root_scan_components` | 500 000 | Hosted root scan stops after this many components. |
| `blocking_permits` | 8 | Parses and renders of a whole index at once. The permit is at least 1. |
| `blocking_wait` | 5 s | Then 503, `Retry-After: 1`, body `the server is busy building indexes, retry`. |
| `sidecar_read_concurrency` | 16 | Parallel sidecar reads while a hosted project page is built. A value of 0 is treated as 1. |
| `sidecar_cache_capacity` | 100 000 | Parsed `.info.json` records and yank reasons, keyed by blob id. A blob does not change, so an entry cannot go stale. |

## 4. Limits and metrics

| Limit | Value | Where |
|---|---|---|
| Project name | 128 bytes | `MAX_NAME_LEN` |
| Version text | 128 bytes, 32 release segments, 32 local segments, each number fits in `u64` | `version` |
| File name | 255 bytes | `MAX_FILENAME_LEN` |
| Retained form field | 4096 bytes | `MAX_FIELD_BYTES` |
| Dropped form fields before the file | 16 MiB | `MAX_DROPPED_FORM_BYTES` |
| Part header | 16 KiB | `MAX_PART_HEADER_BYTES` |
| Multipart boundary | 70 characters, RFC 2046 set, not empty, not ending with a space | `boundary_from_content_type` |
| Yank reason | 1024 bytes, no control characters | `MAX_YANK_REASON_BYTES` |
| Sidecar read back | 64 KiB | `MAX_SIDECAR_BYTES` |
| Wheel `METADATA` | 8 MiB uncompressed and 8 MiB compressed | `zipmeta` |
| ZIP central directory | 16 MiB, at most 200 000 entries, single disk, tail window 128 KiB | `zipmeta` |
| Metadata header lines | 20 000 | `metadata` |
| `Requires-Python` | 256 bytes | metadata and the form |
| Files in one parsed project page | 50 000 | `MAX_FILES_PER_PAGE` |
| Projects in one parsed root index | 1 000 000 | `MAX_PROJECTS_PER_INDEX` |
| Rendered index document | `Limits::max_document_bytes` | handler |
| Upload body idle | `Limits::body_idle_timeout` | multipart reader |

Rendering and parsing of a whole page run on this crate's blocking pool, permit taken before
`spawn_blocking`. Hashing of an upload stays on the task that moves the bytes. ZIP metadata is
at most four ranged reads (tail, central directory, local header, entry).

Metrics through the facade (bounded labels only, no repository, project or file name):

| Metric | Labels |
|---|---|
| `pypi_upload_rejected_total` | `reason`: `form`, `filename`, `mismatch`, `digest`, `metadata`, `exists`, `forbidden`, `other` |
| `pypi_index_requests_total` | `kind`: `root`, `project`, `json_api`; `source`: `hosted`, `proxy`, `group` |
| `pypi_index_too_large_total` | `source` |
| `pypi_yank_total` | `op`: `yank`, `unyank` |
| `pypi_proxy_cache_evicted_total` | none |
| `pypi_proxy_entry_unreachable_total` | none |
| `pypi_wheel_metadata_total` | `result`: `ok`, `invalid`, `unavailable` |

`mismatch` is a form or file name disagreement (`does not match`). `digest` is
`DigestMismatch`. `forbidden` is 401 or 403. Upload, proxy cache, singleflight and download
series are format-core's. Yank state that is only read from an index does not increment
`pypi_yank_total`.

## 5. Negative requirements and their tests

| Requirement | Test |
|---|---|
| No traversal in any encoding | `route::must_not_accept_traversal_in_any_encoding` |
| One normalized spelling of a project name | `name::normalizes_like_pep_503`, `name::spellings_of_one_name_agree`, `route::project_pages_are_canonicalized_and_carry_the_coordinate` |
| A file name cannot carry a path, a NUL or a second project | `filename::refuses_what_is_not_a_distribution_of_this_repository`, `route::unknown_and_inconsistent_paths_are_404` |
| A declared MD5 or BLAKE2-256 that does not match is not a clean end of the file stream | `upload::the_file_stream_yields_exactly_the_file_and_verifies_digests` |
| A truncated upload is not a complete file | `upload::must_not_end_the_file_stream_cleanly_when_the_body_was_cut`, `multipart::must_not_treat_a_truncated_body_as_complete` |
| The file is not read before the fields that place it | `form::must_not_read_the_file_before_the_fields_that_place_it` |
| The form and the file name must name the same project, version and type | `upload::the_form_and_the_file_name_must_agree` |
| Ignored form fields cannot grow without a bound | `form::must_not_keep_unbounded_ignored_fields` |
| A stalled client and an oversized part header are refused | `multipart::must_not_wait_forever_for_a_stalled_client`, `multipart::must_not_accept_an_oversized_part_header` |
| Anonymous upload without the repository flag is 401, with the Basic challenge, even if the authorizer allows it | `safety::must_not_accept_an_anonymous_upload_without_the_repository_flag` |
| A yank reason with a control character or over the cap is refused; a bad stored marker still yanks | `sidecar::reasons_are_validated_and_trimmed`, `sidecar::markers_carry_the_reason_and_a_bad_marker_still_yanks` |
| A hostile `yanked` value is not read as "not yanked" | `simple_json::must_not_read_a_hostile_yanked_value_as_not_yanked` |
| HTML without a base does not follow a relative or non-http link | `simple_html::must_not_follow_a_relative_or_non_http_link_without_a_base` |
| A proxy file URL cannot leave `remote_url` | `proxy::upstream_targets_must_stay_below_remote_url` |
| A proxy member does not add files of a name a hosted member holds, even when the proxy document is first | `group::must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_holds` |
| An unreadable member does not take the group down | `group::an_unreadable_member_is_skipped_and_all_unreadable_is_unprocessable` |
| Wheel metadata is not inflated past the declared size or the size cap | `zipmeta::must_not_inflate_past_the_declared_size`, `zipmeta::must_not_read_a_metadata_entry_over_the_size_bound` |
| No unbounded queue of index work | `blocking::must_not_queue_without_bound_when_no_permit_frees_up` |
| 401 carries a challenge, 403 does not | `error::a_401_challenges_and_a_403_does_not` |
| The parsers do not panic on arbitrary input | proptests `parse_never_panics` in `name`, `version`, `filename`, `route` (`classify_never_panics`), `metadata`, `simple_html`, `simple_json`, `form`, `multipart` |

## 6. ADRs

1. **The normalized name is the only spelling that is stored or authorized.** `ProjectName`
   always holds the PEP 503 form. A look-alike (`Foo_Bar`, `foo.bar`) cannot be a second
   project, a second cache key or a second claim. The canonical path is rebuilt in `classify`
   so `handle` never sees the raw spelling.
2. **The upload is checked before the file is stored, and the file is a stream.** Fields are
   bounded, the coordinate is authorized, and an existing name is 409 before the file bytes are
   read. MD5 and BLAKE2-256 are hashed on the stream. The alternative, buffering the file to
   check it, is what the multipart reader exists to avoid.
3. **SHA-256 is the pipeline's expectation; MD5 and BLAKE2-256 are checked here.** The hosted
   pipeline already verifies one expected checksum. The other two algorithms are not that
   checksum, so they are hashed beside it. A mismatch aborts the staged blob.
4. **Wheel metadata that disagrees with the file name is not kept.** The file is published by
   the pipeline and then checked. On failure the file and its records are deleted. The window
   in section 2.2 is the cost of not inventing a path the audit log would name and then drop.
5. **Yank state is a marker asset, created and deleted, never rewritten.** `allow_once` refuses
   a second write of the same path. A shared mutable document would also lose one of two
   concurrent reasons. An existing marker answers 200 with the stored reason.
6. **Index pages are derived, not stored.** Hosted pages come from the component and asset
   rows. A proxy page is the upstream document rewritten onto this origin. A group page is the
   merge. An uploaded index is not a route.
7. **A proxy never sends the client to the upstream, and never fetches outside `remote_url`.**
   Every file URL in a served page points at this repository. The upstream URL of a file must
   be the same origin as `remote_url` and below its path. A second allow-listed origin is not
   implemented (section 7).
8. **Dependency confusion is a claim and a merge rule.** The claim stops the group pipeline
   from asking later proxy members. The merger drops non-hosted documents when any member holds
   the claim, so a proxy listed first cannot add versions to the page. File URLs still follow
   member order.
9. **HTML is tokenized, not parsed as XML.** Upstream indexes are not well formed XML. Bounds
   (anchors, projects) replace strictness. Non-http links are dropped.
10. **The JSON API is a view of the simple page.** It cannot disagree with the index, and it
    cannot grow fields the page does not have. `last_serial` stays 0.
11. **This crate has its own blocking pool.** `FormatContext` does not expose the engine
    semaphore. The permit is taken before the spawn; no permit within `blocking_wait` is 503.
12. **Errors are plain text with a Basic challenge.** pip and twine act on the status and print
    the body. The challenge is what makes them send the credentials they hold (an API token is
    the password of the user `__token__`).

## 7. Deviations and open issues

* An upstream that lists files on another host cannot be proxied. Warehouse does this
  (`files.pythonhosted.org`). The proxy answers 502 for those files and does not follow them.
* SHA-512 in an upstream hash map is ignored. SHA-256, SHA-1 and MD5 are the expectations the
  pipeline can compute, in that order.
* Between publishing a wheel and deleting it after a bad `METADATA`, the file is visible. A
  panic of the finalizer task skips the compensating delete.
* `GET` of `.yanked` on a proxy or a group is 404. Yank state for those repositories is only
  what the index page says.
* The group merger's file and project caps ignore `PypiConfig`. Changing the handler config
  does not change a merged page's cap.
* `If-Modified-Since` is not evaluated on index documents. Only `If-None-Match` yields 304.
* The 406 response has no `x-request-id`.
* `pyversion` is accepted and discarded. `gpg_signature` is discarded.
* The JSON API has no summary, classifiers, license or serial.
* There is no per-repository version policy and no `format_config`.
* Anonymous upload is refused by the engine when `allow_anonymous` is off. This handler does
  not add a second anonymous check. A repository with `allow_anonymous` and an authorizer that
  allows the write will accept the upload.
* MD5 and BLAKE2-256 are compared with ordinary slice equality, not a constant-time compare.
* A hosted project with more files than `max_index_files` is 502 for every client, not a
  truncated page. The root index can be truncated, and says so.
* Sorting of file entries uses `slice::sort_by` on versions. Version order in the tests is a
  total order on the values the parser accepts.

## 8. Running the tests

```
cargo fmt -p rustryx-format-pypi
cargo clippy -p rustryx-format-pypi --all-targets --locked -- -D warnings
cargo test -p rustryx-format-pypi --all-targets --locked
```

Tests live in `src/` (unit and proptest) and `tests/safety.rs` (anonymous upload through
`FormatEngine`). There is no `tests/` client suite and no fuzz target. No test reads
`RUSTRYX_IT`. A real pip or twine run is not part of this crate; do not set that variable for
these tests.

## 9. For format-core and the harness

Worked around, not changed:

* `FormatContext`'s blocking semaphore is private, so index parse and render use a second pool
  sized by `PypiConfig::blocking_permits`. A `FormatContext::run_blocking` would share the
  engine's pool.
* `ProxyFetch` addresses one URL under `remote_url`. A file index on a second origin needs an
  allow-listed extra origin before this proxy can mirror Warehouse.
* The engine turns an anonymous caller into 401 when `allow_anonymous` is off, before `handle`.
  The upload then authorizes again with the coordinate from the body, which `classify` cannot
  see. A deny at that point is 403, including when the authorizer's error is not `Deny`.
* `HashAlgo` has no BLAKE2-256 and no SHA-512, so those digests cannot be an
  `ExpectedChecksum`. BLAKE2-256 is checked in this crate. SHA-512 is not checked.
* `FormatError` has no 406, so the PEP 691 response is built by hand and does not pass through
  `render_error`.
* Group file resolution is first member wins. The claim removes later proxies only. Hosted
  members have to be listed first for a direct file URL to stay inside the claim.
