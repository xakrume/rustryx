# rustryx-format-core

The shared engine under every format crate (maven, npm, pypi, oci, helm, raw, pub).
A format crate implements one trait, `FormatHandler`, and gets the hosted, proxy and
group behaviour, the SSRF guard, the resilience stack and the security invariants
from here. Format authors write protocol code, not plumbing.

Dependency direction: `api -> formats -> format-core -> core`. This crate depends on
`rustryx-core` only (ports and entities). It never touches PostgreSQL, S3 or a
concrete authorizer: everything arrives as `Arc<dyn Port>` through [`Ports`].

## 1. Request life cycle

```
HTTP layer
  builds RequestHead (method, raw path below the repository, headers, principal, base url)
  FormatEngine::serve(repository, head, body)
    1. handler = registry.get(repository.format)
    2. repository offline?                        -> 503
    3. handler.classify(repo, head)               -> ArtifactRequest   (BEFORE the body is read)
         normalized path, Action, optional Coordinate, ContentKind
    4. write/delete on proxy or group?            -> 405              (body never read)
    5. authorize(request)                         -> deny by default, fail closed
    6. routing rules (proxy, group)               -> 404 when blocked
    7. handler.handle(FormatContext, request, body)
         hosted:  cx.hosted().upload / download / delete
         proxy:   cx.proxy().fetch_artifact / fetch_document
         group:   cx.group().resolve
    8. any FormatError -> logged once here -> handler.render_error(...)
```

The path that is authorized is the path that is read (SEC-19): `classify` normalizes
once, `handle` only reads `req.path`. Nothing downstream decodes again.

## 2. Public API at a glance

| Item | Purpose |
|---|---|
| `FormatHandler` | The contract: `formats()`, `classify()`, `handle()`, optional `group_merge()`, `group_claims()`, `render_error()`, `hide_inaccessible_repository()` |
| `FormatRegistry` | `RepoFormat -> Arc<dyn FormatHandler>`; one handler may declare several labels (`docker` and `oci`); registration is all or nothing |
| `FormatEngine` (+ builder) | The entry point for the HTTP layer; owns shared state; `serve`, `context`, `drain` |
| `FormatContext` | Per request: repository, origin repository, principal, ports, limits, clock, blob store, shutdown token, task tracker, `hosted()`, `proxy()`, `group()`, `upstream()`, `serve_default()` |
| `Ports` | All repository ports as `Arc<dyn ...>`; `Ports::from_store(store, rbac, audit, clock, blob_stores)` for a store that implements them all |
| `BlobStoreRegistry` | Trait: `store_for(&Repository) -> Result<Arc<dyn BlobStore>, BlobError>`. Implemented by the blob crate |
| `RequestHead`, `ArtifactRequest`, `ContentKind` | The request before and after classification |
| `NormalizedPath`, `PathRules` | Decoded, validated path; traversal in any encoding is rejected |
| `FormatError`, `ErrorInfo` | Typed errors with status, stable code, client-safe message, protocol headers |
| `UploadSpec`, `UploadOutcome`, `OverwriteRule`, `ComponentSpec` | Hosted upload input and result |
| `ProxyFetch`, `UpstreamPath`, `CachedDocument`, `ExpectedChecksum`, `ChecksumSource` | Proxy input and result |
| `GroupIndexMerger`, `GroupNameClaimer`, `NameClaim`, `MergeInput`, `MemberDocument`, `MergedDocument` | Group extension points |
| `Limits` | Tunable limits, defaults per spec |
| `RoutingGate`, `ContentEvents` | Seams for routing rules and content lifecycle events (GC) |
| `proxy::{ssrf, executor, upstream, breaker}` | `UpstreamUrlPolicy`, `UpstreamExecutor`, `UpstreamClient`, `ReqwestUpstreamClient`, `CircuitBreaker` |
| `testing` | `FakeUpstream`, `Reply`, `synthetic_stream` for format crates' own tests |
| `metrics` | Metric name constants |

## 3. Writing a format

Register the handler once in the composition root:

```rust
let mut registry = FormatRegistry::new();
registry.register(Arc::new(MyFormat))?;          // one line per format crate
let engine = FormatEngine::builder(ports, registry)
    .limits(limits)
    .url_policy(policy)                           // SSRF policy from configuration
    .shutdown_token(token)
    .build()?;
```

A path-addressed format (raw, the file parts of maven) is a few lines, because
`classify_path_addressed` and `FormatContext::serve_default` implement the defaults:

```rust
#[async_trait]
impl FormatHandler for RawHandler {
    fn formats(&self) -> &'static [RepoFormat] { &[RepoFormat::Raw] }

    fn classify(&self, repo: &Repository, head: &RequestHead) -> Result<ArtifactRequest, FormatError> {
        classify_path_addressed(repo, head)
    }

    async fn handle(&self, cx: FormatContext, req: ArtifactRequest, body: Body)
        -> Result<Response<Body>, FormatError> {
        cx.serve_default(req, body).await
    }
}
```

A real format overrides what is special:

* **`classify`** is pure and cheap (no I/O). Normalize with `NormalizedPath::parse` or
  `parse_with(raw, &PathRules { allow_encoded_slash: true, .. })` (npm scoped names),
  pick the `Action`, attach a `Coordinate` when it can be derived from the path (content
  selectors see it), call `.index()` for documents a group must merge or the proxy
  caches as metadata. Returning `?` on a `PathError` gives an opaque 400.
* **`handle`** matches on `cx.repository().repo_type`:
  * hosted write: build an `UploadSpec` (path, content type, length, `ComponentSpec`,
    `ExpectedChecksum` for client-declared digests, `OverwriteRule::Never` for
    immutable versions) and call `cx.hosted().upload(spec, body)`.
  * hosted read: `cx.hosted().download(&req)` for stored files; for generated
    documents read `cx.ports().assets/components` and build the response yourself.
  * proxy: `cx.proxy().fetch_artifact(ProxyFetch::new(&req)...)` for files (streamed,
    tee'd), `fetch_document(...)` for index documents (bounded in memory), then rewrite
    URLs in the document before serving it. Pass `.accept(..)`, `.cache_path(..)`,
    `.expect(checksum)` or `.checksum_source(..)` as the protocol needs.
  * group: `cx.group().resolve(&req)`; it calls the member handlers, so the same
    `handle` runs for each member with a member context.
* **URLs inside documents** are built from `cx.origin_repository()` (the repository
  the client addressed), never from `cx.repository()`: while a group resolves a
  member, `repository()` is the member and `origin_repository()` stays the group. Use
  `req.head.repository_url(&name)` with the origin's name and `req.head.base_url`.
* **Index merging**: implement `GroupIndexMerger::merge(MergeInput) -> MergedDocument`
  as a pure CPU function over `input.documents` (group order, only members that
  answered). The ordering rules are yours: the first member that has a version wins,
  `holds_claim` marks the hosted member that owns the package name, and so on. It runs
  on the blocking pool with a 64 MiB total cap, so it may parse JSON or XML freely.
  Return `FormatError::Unprocessable` for a document you cannot read; never panic.
  Return the merger from `FormatHandler::group_merge()`.
* **Name claims** (dependency confusion): implement
  `GroupNameClaimer::claim_for(&ArtifactRequest) -> Option<NameClaim>` returning the
  package the request addresses in the form components are stored (`NameClaim::package`,
  `scoped`, `namespace`), and return it from `FormatHandler::group_claims()`. The group
  then refuses to ask later non-hosted members about a name that an earlier hosted
  member has. Applies to index and artifact requests alike.
* **Errors in the protocol's own shape**: override `render_error` and use
  `error.status()`, `error.code()`, `error.public_message()`, `error.headers()`
  (copy it: `Retry-After`, `Allow`, `Content-Range`). `ErrorInfo` says whether the caller
  was anonymous (pub adds `WWW-Authenticate: Bearer realm="pub"`). Override
  `hide_inaccessible_repository` to return `false` where a hidden repository must answer
  403 (pub) instead of 404.
* **Custom upstream calls** (an OCI token handshake): `cx.upstream().execute(UpstreamCall {..})`.
  It applies the SSRF policy, breaker, concurrency cap, timeouts, retries and
  redirect validation. Never use a client of your own. `Authorization` and `Cookie`
  headers are dropped on a redirect to another origin.

## 4. Hosted pipeline

```
body -> idle timeout -> size limit (declared length, quota, max blob) -> 64 KiB slices
     -> BlobWriter (hashes) -> commit -> digest check -> component -> AssetRepo::publish
```

* **Write policy**: `deny` gives 403; `allow_once` and `OverwriteRule::Never` give 409
  when the path exists; `allow` overwrites. Immutable paths are reserved in process, so
  two concurrent uploads of one path cannot both pass (the second gets 409 at once).
* **Sizes**: a declared length over the budget is refused (413 or 507) before the body is
  read. The budget is the smallest of `max_blob_bytes`, the room left in the repository
  quota (a replaced asset's size is credited) and the room left in the blob store quota.
  The same number is the byte limit of the sink, so actual bytes are enforced while
  streaming and nothing over the limit is written. A body longer than its own
  `Content-Length` is cut at that length (400).
* **Cancellation**: dropping the request future before the last byte aborts the staged
  blob (`BlobSink` aborts on drop, also when the store would keep an abandoned writer).
  Commit and publish run in a task tracked by the context, so a client that disconnects
  in that instant cannot leave a committed blob without a database row.
* **After publish**: the replaced blob is already unreferenced in the database and is
  announced as `ContentEvent::BlobOrphaned`; an audit event (`asset.publish`,
  `asset.overwrite`, `asset.delete`, `asset.rejected`) is recorded. A failing audit sink is
  logged and never undoes a committed upload.
* **Download**: strong `ETag` (quoted SHA-256), `Last-Modified`, `X-Checksum-*`,
  `Accept-Ranges`, single `Range` (+ `If-Range`), `If-None-Match`, `If-Match`,
  `If-Modified-Since`, `If-Unmodified-Since`, `HEAD`. The body is the blob stream, so
  client backpressure reaches the store. `nosniff` and `Content-Security-Policy: sandbox`
  on every artifact. A missing blob object behind an existing asset is a 500, not a 404.
* Declared `Content-Type` is only trusted when well formed, not generic and not an active
  type (html, svg, xml, javascript); otherwise the extension decides.

## 5. Proxy pipeline

Freshness: an asset is fresh while `age < content_max_age` (artifacts) or
`metadata_max_age` (documents). Otherwise:

1. negative cache hit (404 remembered, TTL from `ProxyConfig`, bounded) answers 404;
2. singleflight on `(repository, cache path)`: the first request leads, others follow.
   The fill runs in its **own tracked task**, so the key is released when the fill ends, by
   success, error, shutdown or panic (a guard with `Drop`), and a client that disconnects
   does not stop the fill;
3. the leader gets the upstream stream **tee'd**: each chunk goes to the blob store and
   through a bounded channel (2 chunks) to the client before the next chunk is read, so the
   slowest consumer sets the pace. A client that stalls longer than `client_stall_timeout`
   or has left is detached; the fill goes on alone. If the cache cannot take the data
   (quota, size, store failure) the client is still served uncached;
4. followers wait for the fill and then read the cached copy (`X-Rustryx-Cache: MISS`);
5. when a checksum is expected (`ProxyFetch::expect` or a `ChecksumSource` reading
   response headers) there is **no tee**: the body is written, verified, published, and only
   then served. A mismatch deletes the blob, records `proxy.checksum_mismatch` and answers 502;
6. a fill never replaces an artifact that was published while it ran;
7. on failure an expired copy younger than `stale_max_age` is served with
   `X-Rustryx-Cache: STALE` and `Warning: 110 - "Response is stale"`.

Upstream status mapping: 404 and 410 are a miss (negative cache); 401 and 403 become
**502** and are never forwarded (`forbidden_is_miss()` turns a 403 into a miss for
upstreams that answer 403 for missing objects; a 401 never is a miss); 5xx, 429, 408 are
retried (GET and HEAD only, at most 3 attempts, full jitter, `Retry-After` honoured up to
the cap) and then become 502 or 504.

Resilience (`UpstreamExecutor`, applied to every upstream call): SSRF check of the URL,
circuit breaker per host (opens above 50% failures in 30 s with at least 20 requests, one
probe after 30 s), per-repository `Semaphore` (permit before the call, released with the
body, not held while backing off, 503 + `Retry-After` when none frees up), connect timeout
3 s (client), response head and idle timeout 60 s for data, total 30 s for metadata, no
total ceiling for data, redirects followed at most 3 times with each hop re-validated and
credentials dropped across origins. `HEAD` on a miss is an upstream `HEAD` and caches nothing.

### SSRF guard

`UpstreamUrlPolicy` (`Default` denies every host):

* scheme `https`; `http` only for hosts in `http_hosts`; no userinfo; optional host and
  port allowlists (`*.example.com` matches subdomains only);
* every IP literal (the URL parser has folded decimal, octal and hex forms) and every
  DNS answer is checked: loopback, private, link-local, CGNAT, multicast, unspecified,
  broadcast, reserved, documentation, benchmarking, and the cloud metadata addresses, for
  IPv4 and IPv6 including IPv4-mapped, compatible, NAT64 and 6to4 forms;
* `allow_cidrs` opens named internal upstreams; metadata, multicast, unspecified and
  broadcast can never be allowlisted;
* `GuardedResolver` is the HTTP client's resolver: resolve once, validate the whole answer
  (one bad address rejects it), and hand exactly those addresses to the connector. There
  is no second lookup between check and connect, so rebinding cannot swap the target. TLS
  server name and `Host` stay the original name;
* proxy environment variables are ignored, redirects are off in the client (the executor
  follows them), automatic decompression is off (stored bytes are the bytes the checksums
  describe), certificate verification cannot be disabled.

## 6. Group pipeline

* Artifacts: the first member that has the path answers; the body is streamed untouched.
* Index documents (a merger exists): all authorized members are asked (bounded fan-out),
  documents are read under a shared 64 MiB cap and merged on the blocking pool. Client
  validators and ranges are not forwarded to members, `If-None-Match` is evaluated against
  the merged `ETag`.
* Authorization per member through `RbacChecker` with the member's own name; a member the
  caller may not read is skipped silently. Anonymous callers only reach members with
  `allow_anonymous`. Routing rules are applied per member.
* A member's 404, 401 and 403 are discarded; 5xx, timeouts, 429 are skipped but
  remembered: if nobody has the content the group answers with that failure, not 404. A
  client error that applies to the request itself (416, 412, 400) ends the search.
  `X-Rustryx-Group-Skipped: N` marks an answer that may be incomplete; incomplete merges
  are `Cache-Control: no-store`.
* Nested groups: depth limit (`max_group_depth`), cycles and self-membership skipped,
  unknown, offline and wrong-format members skipped.

## 7. Limits and defaults

| Limit | Default | Where |
|---|---|---|
| `max_blob_bytes` | 5 GiB | upload size, cache fill size |
| `max_document_bytes` | 64 MiB | cached index document |
| `group_merge_cap_bytes` | 64 MiB | total of member documents in one merge |
| `max_group_depth`, `max_group_members`, `group_fanout` | 4, 64, 8 | group |
| `body_idle_timeout` | 60 s | client request body |
| `client_stall_timeout` | 60 s | tee client |
| `singleflight_max_wait` | 30 min | follower safety ceiling (fills end by idle timeout) |
| `stale_max_age` | 30 days | serve-stale |
| `negative_cache_capacity` | 100 000 | negative cache |
| `blocking_permits`, `blocking_permit_wait` | 8, 5 s | merge on the blocking pool |
| executor: connect / metadata / idle | 3 s / 30 s / 60 s | `ExecutorConfig`, `ReqwestClientConfig` |
| executor: retries | 3 attempts, 200 ms base, 5 s cap, full jitter | `RetryPolicy` |
| breaker | 30 s window, 20 requests, 50%, 30 s open | `BreakerConfig` |

## 8. Error mapping

| FormatError | Status | Code |
|---|---|---|
| `BadRequest`, `DigestMismatch` | 400 | `bad_request`, `digest_mismatch` |
| `Unauthenticated`, `Forbidden`, `NotFound` | 401, 403, 404 | |
| `MethodNotAllowed` (+`Allow`) | 405 | |
| `RequestTimeout`, `Conflict`, `PreconditionFailed` | 408, 409, 412 | |
| `PayloadTooLarge`, `RangeNotSatisfiable` (+`Content-Range`), `Unprocessable` | 413, 416, 422 | |
| `TooManyRequests` (+`Retry-After`), `QuotaExceeded` | 429, 507 | |
| `Upstream` timeout, other kinds | 504, 502 | `upstream_timeout`, `upstream_*` |
| `IndexTooLarge` | 502 | `index_too_large` |
| `Unavailable` (+`Retry-After`), store/blob unavailable | 503 | |
| other store/blob/internal | 500 | `internal` |

`public_message()` never contains SQL text, paths or upstream addresses; those stay in
`Display` and the log. The generic body is `{"error":{"code","message","request_id"}}`.

## 9. Metrics

Emitted through the `metrics` facade (names in `metrics::*`). The repository name is a
label only on `upstream_*` (cardinality rule SRE-09).

`upstream_requests_total{repo,status}`, `upstream_request_duration_seconds{repo}`,
`upstream_errors_total{repo,kind}`, `upstream_circuit_state{repo}` (0 closed, 1 half-open,
2 open), `proxy_cache_total{format,result}` (`hit`, `miss`, `stale`, `negative`),
`singleflight_total{role}` (`leader`, `follower`: followers are the waiters),
`hosted_uploads_total{format,result}`, `hosted_upload_bytes_total{format}`,
`group_member_skipped_total{reason}`, `proxy_client_detached_total`,
`blob_orphan_leak_total`, `blob_writer_abandoned_total`, `hosted_allow_once_violation_total`.

## 10. Negative requirements and their tests

Every row is a `must_not_*` test (or a test whose name states the property).

| Requirement | Test |
|---|---|
| One upstream request for N concurrent misses | `proxy::must_not_fetch_upstream_n_times_for_n_concurrent_misses`, `real_http::thirty_concurrent_clients_cause_one_real_request` |
| Singleflight key removed on success, error, cancel, panic | `flight::must_remove_key_*`, `proxy::followers_get_the_failure_and_the_key_is_removed_on_error`, `proxy::a_panicking_fill_releases_the_key_and_the_request_recovers` |
| Client disconnect does not cancel the fill, nor replace a published artifact | `proxy::must_not_cancel_the_fill_*`, `proxy::must_not_replace_an_artifact_that_was_published_while_the_fill_ran` |
| No partial blob on error or cancellation | `hosted::must_not_leave_a_partial_blob_*`, `hosted::an_abandoned_writer_is_aborted_*`, `hosted::an_upload_cancelled_right_after_the_commit_*` |
| Body never buffered whole | `memory::must_not_buffer_whole_artifact_in_memory` (RSS growth under 64 MiB; 2 GiB checked by hand) |
| No data through a group without member rights | `group::must_not_leak_data_through_group_without_member_rights`, `..._when_its_authorization_lookup_fails`, `group::merge_only_includes_members_the_caller_may_read` |
| Dependency confusion | `group::must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has`, `group::the_claim_holds_even_when_the_caller_cannot_read_the_claiming_member`, `group::a_failing_claim_lookup_fails_closed` |
| Upstream and member 401/403 never forwarded | `proxy::must_not_forward_upstream_401_or_403_to_the_client`, `group::must_not_forward_a_member_auth_failure_through_the_group`, `fill::must_not_turn_upstream_auth_failures_*` |
| SSRF classes (loopback, private, link-local, metadata, CGNAT, multicast, unspecified, broadcast, reserved, IPv4-mapped, NAT64, 6to4, alternate notations, userinfo) | `ssrf::must_not_*` |
| No connection to internal addresses, rebinding, one DNS lookup | `real_http::must_not_connect_to_loopback_unless_the_policy_allows_it`, `must_not_connect_when_a_dns_answer_mixes_*`, `resolves_once_and_connects_to_the_validated_address` |
| Redirects re-validated, credentials stay on their origin | `executor::must_not_follow_redirects_to_unlisted_hosts_or_internal_addresses`, `must_not_follow_more_than_three_redirects`, `must_not_forward_format_set_credentials_to_another_origin`, `real_http::must_not_send_credentials_to_another_origin_after_a_redirect` |
| No client headers forwarded upstream | `proxy::must_not_forward_client_headers_to_the_upstream` |
| Wrong checksum neither served nor cached | `proxy::must_not_serve_or_cache_content_with_a_wrong_checksum` |
| Truncated or broken upstream body not cached, client stream breaks | `proxy::must_not_cache_a_truncated_body_*`, `real_http::a_connection_dropped_mid_body_*` |
| Retries only for GET and HEAD, at most 3, never 4xx other than 408 and 429 | `executor::must_not_retry_*`, `executor::retries_idempotent_requests_at_most_three_times` |
| No permit held while backing off | `executor::must_not_hold_the_permit_while_backing_off` |
| Open breaker sends nothing; cancelled probe does not wedge it | `breaker::must_not_send_requests_while_open_*`, `must_not_stay_half_open_*` |
| No traversal in any encoding, authorization never sees a bad path | `path::must_not_*`, `hosted::must_not_resolve_traversal_paths_*`, `target::must_not_leave_the_configured_origin_or_prefix` |
| Anonymous needs the repository flag | `hosted::must_not_allow_anonymous_reads_without_the_repository_flag` |
| Oversized body or declared size refused before reading | `hosted::must_not_read_the_body_when_the_declared_size_exceeds_the_limit`, `..._more_bytes_than_the_limit_...` |
| Immutable path cannot be written twice concurrently | `hosted::must_not_allow_two_concurrent_uploads_of_an_immutable_path` |
| Active content types not stored from client claims | `mime::must_not_store_active_content_types_declared_by_client`, `hosted::must_not_store_a_declared_html_content_type` |
| Self-signed upstream certificate refused | `tls::must_not_accept_a_self_signed_upstream_certificate` |
| No internal text in error bodies; no credentials in `Debug` | `error::must_not_leak_internal_text_in_public_message`, `request::must_not_leak_credentials_in_request_head_debug`, `proxy::credentials_set_by_the_format_*` |
| Merge memory cap | `group::must_not_buffer_more_than_the_merge_cap` |

## 11. ADRs

1. **`axum_core::body::Body`** as the body type. Alternative: `axum::body::Body` or
   `http_body_util::BoxBody`. `axum::body::Body` is a re-export of the axum-core type, so the
   API crate needs nothing else, and format crates do not pull the router, `hyper` server
   and `tower` stack. A custom body type would force a conversion at every boundary.
2. **The engine owns the order** classify, 405 for non-hosted writes, authorize, routing,
   handle. Alternative: the API layer does it per route. A rule that every format and
   every route must remember is a rule that gets forgotten; here it holds in one function.
3. **The fill is a task of its own**, followers wait for completion. Alternatives: fill in
   the leader's future (a disconnect would cancel it, README 8.1 forbids that) or a
   broadcast tee to every waiter (late joiners cannot join, slow waiters need a lag policy).
   Cost: a follower's first byte comes after the fill finished; bounded by the upstream idle
   timeout.
4. **No tee when a checksum is expected.** "Verify before serving" and "stream while
   caching" cannot both hold; the format chooses by supplying an expectation.
5. **Resilience above a minimal `UpstreamClient`** (`send` one request, no redirects, no
   retries). Test doubles script answers; breaker, retries, redirects and the cap are tested
   once on the executor. DNS validation lives in the resolver of the real client because
   that is the only place where check and connect are one step; the executor validates
   URLs and redirect targets for any client.
6. **A DNS answer with any forbidden address is rejected whole**, not filtered. A mixed
   answer is what rebinding looks like. Consequence: a dual-stack internal name needs both
   families in `allow_cidrs`.
7. **Commit and publish in a tracked task; `BlobSink` aborts on drop.** The `BlobWriter`
   contract makes commit non-cancel-safe, and a committed blob has no database trace.
8. **Index documents are the only buffered data**, under explicit limits; merges run on
   the blocking pool, permit taken before spawn, one atomic counter caps the memory of all
   members together.
9. **Name claims live in the group pipeline** and use `ComponentRepo`: hosted members before
   a non-hosted member claim against it, regardless of the caller's rights, failing closed.
   Alternative: let the merger decide. A merger cannot stop a direct artifact URL.
10. **`BlobStoreRegistry::store_for` is fallible** (the task text showed a plain
    `Arc<dyn BlobStore>`). A repository can reference a deleted or failed store and panics are
    forbidden; an infallible signature would force a fake store.
11. **In-process reservation for immutable paths**, because `AssetRepo::publish` is an upsert.
    Across nodes the race stays open (counted in `hosted_allow_once_violation_total`).
12. **Group responses expose only a count of skipped members** (`X-Rustryx-Group-Skipped`),
    never member names: a caller learns that an answer may be incomplete, not which
    repository failed or which one it may not read.

## 12. Known gaps and follow-ups

For `rustryx-core` (not edited here):

* `AssetRepo::publish` needs a create-only mode (race-free `allow_once`, race-free "fill
  never replaces") and a way to refresh `updated_at` after an upstream 304 (cheap
  revalidation). Today an expired artifact is fetched again in full.
* No port for routing rules; `RoutingGate` is the seam until one exists.
* A committed blob that was never published has no database row, so GC cannot find it after
  a crash between commit and publish. Needs a store sweep (keys not in `blobs`) or a
  two-phase row.
* `BlobStore` has no resumable or composable upload, which OCI chunked uploads across
  nodes need.
* `ProxyConfig` has no per-repository CA bundle (SEC-27 allows one) and no stale limit.

In this crate:

* Per-repository CA bundles are not supported (one shared HTTP client).
* A slow client holds an upstream concurrency permit while it is tee'd; tune
  `client_stall_timeout` and the per-repository cap.
* Revalidation of expired cache entries is a full refetch (see above).
* The blob store definition (for quotas) is read on every write; wrap the port in a cache
  if that shows up in profiles.
