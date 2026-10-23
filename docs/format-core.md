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
    6. routing rules (proxy, group)               -> 404 when blocked (RuleRoutingGate)
    7. handler.handle(FormatContext, request, body)
         hosted:  cx.hosted().upload / download / delete / begin_chunked ...
         proxy:   cx.proxy().fetch_artifact / fetch_document
         group:   cx.group().resolve
    8. any FormatError -> logged once here -> handler.render_error(...)
    9. the response leaves with RequestLabels (class, cache) as a response extension
```

The path that is authorized is the path that is read (SEC-19): `classify` normalizes
once, `handle` only reads `req.path`. Nothing downstream decodes again.

## 2. Public API at a glance

| Item | Purpose |
|---|---|
| `FormatHandler` | The contract: `formats()`, `classify()`, `handle()`, optional `group_merge()`, `group_claims()`, `render_error()`, `hide_inaccessible_repository()` |
| `FormatRegistry` | `RepoFormat -> Arc<dyn FormatHandler>`; one handler may declare several labels (`docker` and `oci`); registration is all or nothing |
| `FormatEngine` (+ builder) | The entry point for the HTTP layer; owns shared state; `serve`, `context`, `drain`, `tasks` (the tracker that `drain` waits for) |
| `FormatContext` | Per request: repository, origin repository, principal, ports, limits, clock, blob store, shutdown token, task tracker, `hosted()`, `proxy()`, `group()`, `upstream()`, `serve_default()` |
| `Ports` | All repository ports as `Arc<dyn ...>`, plus the routing gate, the event sink and the policy hook; `Ports::from_store(store, rbac, audit, clock, blob_stores)` for a store that implements them all |
| `BlobStoreRegistry` | Async trait, implemented by the blob crate. `store_by_id(BlobStoreId)` is required; `store_for(&Repository)` (new writes) and `store_for_blob(&BlobRecord)` (existing content) are provided |
| `RequestHead`, `ClientInfo`, `ArtifactRequest`, `ContentKind` | The request before and after classification; `ClientInfo::download_source` says who causes a read (replication, internal) when it is not a client |
| `NormalizedPath`, `PathRules` | Decoded, validated path; traversal in any encoding is rejected |
| `FormatError`, `ErrorInfo` | Typed errors with status, stable code, client-safe message, protocol headers |
| `UploadSpec`, `UploadOutcome`, `OverwriteRule`, `ComponentSpec` | Hosted upload input and result |
| `ProxyFetch`, `UpstreamPath`, `CachedDocument`, `ExpectedChecksum`, `ChecksumSource` | Proxy input and result |
| `GroupIndexMerger`, `GroupNameClaimer`, `NameClaim`, `MergeInput`, `MemberDocument`, `MergedDocument` | Group extension points |
| `Limits` | Tunable limits, defaults per spec |
| `RoutingGate`, `RuleRoutingGate`, `ContentEvents` | The routing seam, its implementation over `RoutingRuleRepo`, and the content event sink |
| `ContentPolicy`, `WriteIntent`, `DeleteIntent`, `NoopContentPolicy` | The pre check hook of hosted writes and deletes (immutability, holds, quota headroom plug in here) |
| `ChunkedSession` and `HostedPipeline::{supports_chunked, begin_chunked, append_chunk, chunked_length, finish_chunked, abort_chunked}` | Uploads that span requests and nodes, on stores with the resumable capability |
| `DownloadBatcher` | `ContentEvents` sink that folds client downloads into `AssetRepo::record_downloads` batches |
| `RequestLabels`, `RequestClass`, `CacheLabel` | The `class` and `cache` labels of `http_request_duration_seconds`, put on every response |
| `proxy::{ssrf, executor, upstream, breaker}` | `UpstreamUrlPolicy`, `UpstreamExecutor`, `UpstreamClient`, `ReqwestUpstreamClient`, `CircuitBreaker` |
| `testing` | `FakeUpstream`, `Reply`, `synthetic_stream` for format crates' own tests |
| `metrics` | Metric name constants and the label helper |

## 3. Writing a format

Register the handler once in the composition root:

```rust
let mut registry = FormatRegistry::new();
registry.register(Arc::new(MyFormat))?;          // one line per format crate
let mut ports = Ports::from_store(store, rbac, audit, clock, blob_stores);
ports.routing = Arc::new(RuleRoutingGate::new(routing_rules));  // routing rules (section 5.1)
ports.policy = Arc::new(my_policy);                              // immutability, holds, quota headroom
let batcher = DownloadBatcher::new(event_sink, DownloadBatcher::DEFAULT_MAX_PENDING);
ports.events = batcher.clone();                                  // batched download counters
let engine = FormatEngine::builder(ports, registry)
    .limits(limits)
    .url_policy(policy)                           // SSRF policy from configuration
    .shutdown_token(token)
    .build()?;
batcher.spawn_flusher(assets, Duration::from_secs(10), engine.shutdown_token().clone(), engine.tasks());
```

The HTTP layer reads `RequestLabels::of(&response)` after `serve` and records
`http_request_duration_seconds` with its `class` and `cache` labels (section 9).

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
  caches as metadata. `.index()` is also what puts the request in the `metadata` latency
  class (p95 below 300 ms), so mark exactly the documents that objective is about: an
  artifact marked as an index is judged by the wrong number and is never counted as a
  download. Returning `?` on a `PathError` gives an opaque 400.
* **`handle`** matches on `cx.repository().repo_type`:
  * hosted write: build an `UploadSpec` (path, content type, length, `ComponentSpec`,
    `ExpectedChecksum` for client-declared digests, `OverwriteRule::Never` for
    immutable versions) and call `cx.hosted().upload(spec, body)`.
  * hosted chunked write (OCI blob uploads): ask `cx.hosted().supports_chunked()` before
    handing out an upload location, then `begin_chunked()` (the `ChunkedSession::token()` goes
    into the location), `append_chunk(&session, offset, body, declared_length)` per `PATCH`
    (the answer to an out of step offset is `FormatError::RangeNotSatisfiable { total }`, which
    renders the `Range` the client resumes from), `finish_chunked(session, spec)` for the
    closing `PUT` and `abort_chunked` for `DELETE`. `ChunkedSession::parse(token, repository_id)`
    gives the session back on any node and refuses a token of another repository.
  * hosted delete: `cx.hosted().delete(&path)` returns the blob that lost its last
    reference, `None` when another asset (a linked copy) still references it.
  * hosted read: `cx.hosted().download(&req)` for stored files; for generated
    documents read `cx.ports().assets/components` and build the response yourself.
    Blob stores are reached with `cx.store().await` (new writes) and
    `cx.store_for_blob(&record).await` (existing content, which for a repository on a group
    store is the member that holds the bytes).
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
write policy -> policy hook -> body -> idle timeout -> size limit (declared length, quota, max blob)
     -> 64 KiB slices -> BlobWriter (hashes) -> commit -> digest check -> component
     -> AssetRepo::publish_with (create-only for immutable paths)
```

* **Write policy**: `deny` gives 403; `allow_once` and `OverwriteRule::Never` give 409
  when the path exists; `allow` overwrites. A path that must stay immutable is published
  with `PublishMode::CreateOnly`, so the database refuses the second writer
  (`PolicyViolation::AlreadyExists`, 409) whichever node it runs on and whenever the other
  upload finished: the guarantee is the transaction, not this process. The in-process
  reservation of immutable paths stays as an optimisation only: the second of two concurrent
  uploads on one node is refused at once, before its body is read. A refused late upload
  deletes its committed object.
* **Policy hook** (`ContentPolicy`, `Ports::policy`, default `NoopContentPolicy`): called
  after the write policy and before the body is read, with a `WriteIntent` (repository,
  principal, path, declared length, existing asset, component), and before a delete with a
  `DeleteIntent`. It refuses with `PolicyViolation` and cannot decide with
  `MutationError::Store`; both stop the request before a byte is read or a blob is staged, and
  an undecidable hook fails the request closed (503), never open. Immutability rules, lifecycle
  holds and the quota pre check (`QuotaRepo::headroom` with the declared length) implement it.
  It is advisory: `publish_with` and `delete_by_path_checked` enforce the same policies in the
  transaction that switches the asset and are the authority. A refusal from either is counted
  (`policy_violations_total{format,reason}`) and audited (`asset.rejected`, kind
  `SecurityDecision`, the label as reason, never a rule id); the routine refusals of the write
  policy are counted and not audited, so a client that retries an immutable upload cannot fill
  the log. Refusals look the same to the client whichever layer caught them (section 8).
* **Request context of port calls**: `publish_with` and `delete_by_path_checked` get a
  `WriteOptions` with the uploader as actor, the request id and the mode. `override_immutable`
  is never set here: only an admin driven lifecycle run sets it.
* **Sizes**: a declared length over the budget is refused (413 or 507) before the body is
  read. The budget is the smallest of `max_blob_bytes`, the room left in the repository
  quota (a replaced asset's size is credited) and the room left in the blob store quota.
  The same number is the byte limit of the sink, so actual bytes are enforced while
  streaming and nothing over the limit is written. A body longer than its own
  `Content-Length` is cut at that length (400). The transaction checks the quota again on the
  actual size.
* **Cancellation**: dropping the request future before the last byte aborts the staged
  blob (`BlobSink` aborts on drop, also when the store would keep an abandoned writer).
  Commit and publish run in a task tracked by the context, so a client that disconnects
  in that instant cannot leave a committed blob without a database row.
* **Where bytes live**: a new write goes to `BlobStoreRegistry::store_for(repository)` (for a
  group store, the group adapter), and the blob row names the member in
  `BlobCommit::placement` when the store reports one. Reads, ranges and the cleanup of an
  unpublished object go through the store the row (or the commit) names, never through the
  group, so no read probes members.
* **After publish**: the replaced blob is already unreferenced in the database and is
  announced as `ContentEvent::BlobOrphaned`; an audit event (`asset.publish`,
  `asset.overwrite`, `asset.delete`, `asset.rejected`) is recorded. A failing audit sink is
  logged and never undoes a committed upload. The audit record carries the actor kind and the
  project of the repository.
* **A commit that is not published** (digest mismatch, a refused transaction) deletes its
  object. If that delete fails, the object is an orphan no `blobs` row names:
  `blob_orphan_leak_total` counts it and the log has the key. Core revision 2 rejected a
  pending row for it (it would add a write to every upload and not cover a lost commit
  response of an object store); the store sweep of `verify_blobs` (`BlobStore::list_keys`
  against the rows) collects such objects.
* **Download**: strong `ETag` (quoted SHA-256), `Last-Modified`, `X-Checksum-*`,
  `Accept-Ranges`, single `Range` (+ `If-Range`), `If-None-Match`, `If-Match`,
  `If-Modified-Since`, `If-Unmodified-Since`, `HEAD`. The body is the blob stream, so
  client backpressure reaches the store. `nosniff` and `Content-Security-Policy: sandbox`
  on every artifact. A missing blob object behind an existing asset is a 500, not a 404.
* **Download counting**: a client `GET` that opens the body of an artifact emits
  `ContentEvent::AssetDownloaded` (not `HEAD`, not a `304`, not an index document, not the
  first client of a cache miss, whose copy did not exist yet). The source is `Client` unless
  the principal is a scan token (`Scanner`) or `ClientInfo::download_source` says otherwise
  (replication, internal reads); only `Client` is emitted, so a scanner cannot keep an unused
  artifact alive. `DownloadBatcher` folds the events into one tick per asset and writes them
  with `record_downloads` once per interval; its map is capped (`download_counter_dropped_total`),
  a failed flush keeps its counts and merges them with the next, the final flush at shutdown
  is bounded to 10 seconds.
* Declared `Content-Type` is only trusted when well formed, not generic and not an active
  type (html, svg, xml, javascript); otherwise the extension decides.

### Chunked uploads (resumable stores)

OCI blob uploads span requests and, behind a load balancer, nodes. `BlobStore::resumable()`
(core revision 2, gap 29) stages each chunk as its own object and `finalize` joins and hashes
them; this crate adds the pipeline on top (`supports_chunked`, `begin_chunked`, `append_chunk`,
`chunked_length`, `finish_chunked`, `abort_chunked`):

* a request body is cut into segments of `Limits::chunk_segment_bytes` (8 MiB), one staged chunk
  each, so memory is one segment and not the body;
* the blob size cap and the quota are checked against the staged length before the body is
  read (declared length) and while it streams;
* an offset that is not the staged length is `RangeNotSatisfiable { total }` with the real
  length, so the client resumes from it; a body that breaks leaves the segments staged so far;
* `finish_chunked` ends exactly like `upload`: write policy, policy hook (with the real total,
  at the end: there is no path and no size at the start), create-only publish for immutable
  paths, expected digest, events, audit, deletion of the joined object when the publish is
  refused. `finalize` and the publish run in a tracked task, so a client that disconnects
  during the join cannot leave a committed object without its row;
* the session token is `repository-uuid.blob-uuid.store-handle`. It names the repository, so a
  session opened through one repository cannot be finished through another, and a malformed
  or foreign token is `NotFound`. It is not a secret: it lets a caller who may write to the
  repository continue the upload, nothing more.

A store without the capability answers `supports_chunked() == false`; a caller that did not
probe gets `Blob(Unsupported("resumable"))` (500 class, its own bug).

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
6. a fill of a path that has no copy is published **create-only**: it never replaces an
   artifact that was published while it ran, on any node. The transaction answers
   `AlreadyExists`, the fill deletes its own object and the copy that exists is served. Only a
   fill that refreshes an expired copy may replace it. A fill carries no actor (it is the
   system's doing, not a user's upload) and creates its component without `created_by`. If the
   transaction refuses to store the copy (read only mode, immutability of the expired copy,
   quota), the cache is simply not updated: the tee client already has its data, a
   verified or document fetch is answered with the refusal, or with the expired copy as `STALE`
   when there is one;
7. on failure an expired copy younger than `stale_max_age` is served with
   `X-Rustryx-Cache: STALE` and `Warning: 110 - "Response is stale"`.

### Revalidation of an expired copy

An expired copy is first asked about, not fetched again. The fill sends a conditional `GET`
(`If-None-Match` with the upstream's own `ETag`, `If-Modified-Since` with its own
`Last-Modified`, both verbatim) when it knows the validators of exactly that copy:

* `304`: `AssetRepo::touch_fresh` resets the age, nothing is stored or served from the
  network, the copy is answered as `X-Rustryx-Cache: HIT` and counted in
  `proxy_cache_total{result="revalidated"}`. A `304` to a request that carried no condition is
  a broken upstream: 502, nothing cached;
* `200` with a body that has the digest of the copy (an upstream that cannot answer `304`, or
  a node that had no validators): the new object is deleted and only the age is reset, so the
  blob is not replaced, nothing is announced as orphaned and `Last-Modified` does not change;
* `200` with different content: replaced as before (create-or-replace, the old blob is
  announced).

Where the validators live, honestly: `Asset` has no column for them, so they are kept in a
bounded per node cache (`Limits::validator_cache_capacity`) keyed by `(asset id, blob id)`, the
pair that identifies the bytes. An entry for a replaced blob is never found again, so a
validator is never sent for content it does not describe. A node that did not fill the copy
(restart, another node) asks without a condition and falls back to the digest comparison above.
Nothing is derived from our own timestamps: `If-Modified-Since` with the time we cached the copy
would miss a change made between the upstream's answer and our commit, and an upstream with
one second timestamps would miss changes inside the same second. A core field for the
validators would make revalidation work across nodes and restarts (section 12).

Upstream status mapping: 404 and 410 are a miss (negative cache); 401 and 403 become
**502** and are never forwarded (`forbidden_is_miss()` turns a 403 into a miss for
upstreams that answer 403 for missing objects; a 401 never is a miss); 5xx, 429, 408 are
retried (GET and HEAD only, at most 3 attempts, full jitter, `Retry-After` honoured up to
the cap) and then become 502 or 504.

### 5.1 Routing rules

`RuleRoutingGate` (the `RoutingGate` implementation over `RoutingRuleRepo`) evaluates the rule a
proxy or group repository points at (`Repository::routing_rule_id`) against the normalized path,
before the cache is looked at, so a rule added later applies to cached content at once.

* Patterns are regular expressions of the `regex` crate (linear time, compiled size capped at
  256 KiB) matched **anywhere** in the path (`is_match`): anchor with `^` and `$` for the whole
  path. The path has no leading slash and was decoded once by `classify`.
* `Block`: refused when any pattern matches. `Allow`: refused unless any pattern matches, so
  an allow list blocks every path that matches nothing.
* A refusal is a 404 for the addressed repository and a silently skipped member inside a
  group (indistinguishable from "not there").
* **Precedence**: one rule per repository. Across a group the rules are cumulative and a
  refusal always wins: the engine checks the group's own rule first, the group pipeline checks
  each member's rule before it asks that member, so a member serves a path only when the group
  rule and the member rule both let it through. An allow list on the group never widens a block
  list of a member and the other way round.
* Only proxy and group repositories are gated. A rule on a hosted repository is ignored.
* Fail closed: a rule that cannot be read answers 503 (a group skips the member), a rule id that
  does not resolve, a rule that fails `validate()` and a pattern that does not compile are
  server errors, never an allow.
* Compiled rules are cached per node for 5 seconds (`RuleRoutingGate::with_ttl`, zero reads on
  every request); concurrent misses share one load, a failed load is not cached,
  `invalidate(id)` makes the node that saved a change immediate.

### Resilience

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
| `chunk_segment_bytes` | 8 MiB | size of one staged chunk of a chunked upload, bounds the memory of a `PATCH` |
| `validator_cache_capacity` | 100 000 | upstream validators of cached copies, per node |
| routing rule cache | 5 s | `RuleRoutingGate::with_ttl` |
| download counter map, final flush | 1 000 000 assets, 10 s | `DownloadBatcher` |
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
| `Policy(Immutable)`, `Policy(Held)`, `Policy(ApprovalRequired)` | 409 | `immutable`, `held`, `approval_required` |
| `Policy(ReadOnly)` (+`Retry-After: 30`) | 503 | `read_only_mode` |
| `Upstream` timeout, other kinds | 504, 502 | `upstream_timeout`, `upstream_*` |
| `IndexTooLarge` | 502 | `index_too_large` |
| `Unavailable` (+`Retry-After`), store/blob unavailable, `BlobError::Overloaded` (+`Retry-After`, from the store when it says) | 503 | `unavailable` |
| `StoreError::Invalid` | 400 | `bad_request`, message `invalid request` |
| `BlobError::InsufficientStorage` | 507 | `insufficient_storage` |
| other store/blob (`InvalidKey`, `Unsupported`, unknown variants)/internal | 500 | `internal` |

Conversions: `From<PolicyViolation>` and `From<MutationError>` give the same error whichever
layer refused: a quota refusal is `QuotaExceeded`, `deny` is `Forbidden`, `allow_once` and an
existing path are `Conflict` (the pre check of the pipeline already used these), the others are
`Policy`. `From<rustryx_core::UpstreamError>` and `From<DomainError>` map a core upstream error
through `UpstreamFailureKind::of`, the one place that decides how an upstream failure reaches a
client: a timeout is 504, everything else 502, and an upstream status of **401 or 403 is
`Rejected`, a 502, never forwarded**. The transport level `proxy::upstream::UpstreamError` keeps
what the executor needs (timeout phase for the retry decision, the rejection detail for the audit
record) and projects onto the core type with `to_core()`. Every enum of core that grows has a
wildcard arm that is a 500, never an allow and never a 404.

`public_message()` never contains SQL text, paths, rule or hold ids, or upstream addresses; those
stay in `Display` and the log. The generic body is `{"error":{"code","message","request_id"}}`.

## 9. Metrics

Emitted through the `metrics` facade (names in `metrics::*`). The repository name is a
label only on `upstream_*` (cardinality rule SRE-09). A test pins the names and label sets that
`docs/slo.md` and `deploy/monitoring/rustryx-alerts.yaml` read.

| Series | Labels | Notes |
|---|---|---|
| `upstream_requests_total` | `repo`, `status` | one per attempt; `status` is the code or `error` |
| `upstream_request_duration_seconds` | `repo` | time to response headers |
| `upstream_errors_total` | `repo`, `kind` | `connect`, `timeout`, `5xx`, `circuit_open` (the alert selects it), plus `blocked`, `protocol`, `body`, `overloaded` |
| `upstream_circuit_state` | `repo` | 0 closed, 1 half-open, 2 open. Exported closed when the breaker is created (a healthy circuit is visible, not absent); the breaker is per host, so every repository that calls the host reports the same value |
| `proxy_cache_total` | `format`, `result` | `hit`, `miss`, `stale`, `negative` (a 404 from the upstream and a remembered one), `revalidated` |
| `singleflight_total` | `role` | `leader`, `follower` |
| `hosted_uploads_total`, `hosted_upload_bytes_total` | `format`, `result` / `format` | `result` is `ok` or the error code |
| `policy_violations_total` | `format`, `reason` | `PolicyViolation::label()`: a hook, a pre check or the transaction refused |
| `download_counter_pending`, `download_counter_flush_total{result}`, `download_counter_dropped_total` | | `DownloadBatcher` |
| `group_member_skipped_total` | `reason` | |
| `proxy_client_detached_total`, `blob_orphan_leak_total`, `blob_writer_abandoned_total`, `hosted_allow_once_violation_total` | | the last one is a tripwire that must stay 0: a port ignored `CreateOnly` |

**`http_request_duration_seconds` is recorded by the HTTP layer**, which needs two labels from
this crate for the latency alerts. `FormatEngine::serve` puts `RequestLabels` on every response it
returns (a response extension, nothing on the wire) and the HTTP layer reads it:

```rust
let labels = RequestLabels::of(&response);      // Other / None for a response the engine did not make
histogram!(HTTP_REQUEST_DURATION_SECONDS, "route" => route, "method" => method,
           "class" => labels.class.as_str(), "cache" => labels.cache.as_str())
    .record(elapsed.as_secs_f64());
```

`class` is `metadata` (the format called `.index()`), `download` (a read of an artifact) or
`upload` (a write or a delete); `api` and `other` are for the HTTP layer's own routes and for
requests that failed before they were classified. `cache` is `hit` (served from local storage
without waiting for an upstream: a fresh or revalidated proxy copy, and a hosted read, which is
what the time to first byte objective measures), `miss` (waited for an upstream fill), `stale`,
`bypass` (a merged group index, composed without a cache) or `none` (writes, deletes, errors).
The alerts select `class="metadata"` and `class="download",cache="hit"` only. Two things for
the owners of the alerts: `docs/slo.md` assumption A2 lists the cache values `hit`, `miss`,
`none` and should add `stale` and `bypass`; and `p95_metadata_5m` does not exclude
`cache="miss"`, so a metadata document that waited for its upstream counts against the 300 ms
objective, which section "Latency SLIs" of `docs/slo.md` says is outside it.

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
| A path that must stay immutable is not replaced by an upload racing on another node | `policy::must_not_overwrite_an_allow_once_path_published_by_another_node`, `policy::must_not_replace_a_never_overwrite_path_published_by_another_node` |
| A policy hook that refuses costs no byte of the body; one that cannot decide never allows | `policy::must_not_read_the_body_when_a_policy_hook_refuses`, `policy::must_not_allow_a_write_when_the_policy_hook_cannot_decide`, `policy::must_not_delete_when_the_policy_hook_cannot_decide` |
| An unpublished object is deleted from the store that took it, never through the group adapter | `policy::must_not_delete_an_unpublished_object_through_the_group_adapter`, `policy::blob_rows_name_the_member_that_took_the_bytes_and_reads_go_there` |
| A fill never replaces a copy published meanwhile, on any node | `proxy::must_not_replace_an_artifact_that_was_published_while_the_fill_ran`, `revalidation::fills_publish_create_only_unless_they_refresh_an_expired_copy`, `revalidation::a_fill_that_lost_the_create_race_discards_its_copy_and_serves_the_winner` |
| A validator is never sent for bytes it does not describe; an unasked 304 is not content | `revalidation::must_not_send_the_validators_of_a_replaced_copy`, `revalidation::must_not_forward_an_unasked_304_as_content` |
| A fill the transaction refuses is not stored | `revalidation::must_not_store_a_fill_that_the_transaction_refuses` |
| Upstream 401 and 403 of the core error are not forwarded either | `error::must_not_forward_a_core_upstream_401_or_403` |
| No path served when the routing rule cannot be read, is missing or does not compile | `routing::must_not_serve_a_path_when_the_rule_cannot_be_read`, `routing::must_not_serve_a_path_when_the_rule_is_missing_or_invalid`, `routing::must_not_compile_a_pattern_beyond_the_size_limit` (unit) |
| A chunked upload holds one segment, not the body, and never stages over the limit | `chunked::must_not_buffer_a_patch_body_beyond_one_segment`, `chunked::must_not_stage_a_chunk_over_the_blob_limit`, `chunked::must_not_read_a_chunk_body_when_the_declared_length_exceeds_the_room` |
| Scanner, replication and internal reads are not downloads | `downloads::must_not_count_scanner_reads`, `downloads::must_not_count_replication_or_internal_reads`, `downloads::must_not_count_an_event_that_is_not_a_client_download` |
| The counter flush never double counts and respects its cap | `downloads::must_not_double_count_after_a_failed_flush`, `downloads::must_not_grow_the_counter_map_beyond_its_cap` |

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
10. **`BlobStoreRegistry` is async and its lookups are fallible, `store_by_id` is the required
    one.** A repository can reference a deleted or failed store, panics are forbidden, and a
    registry may open a store on first use. `store_for(&Repository)` (new writes) and
    `store_for_blob(&BlobRecord)` (existing content) are provided on top of it, as core revision 2
    specifies, because the right store depends on the question: a group store takes the write,
    the member named in the row serves the read.
11. **The create-only publish is the guarantee, the in-process reservation an optimisation.**
    `publish_with(PublishMode::CreateOnly)` makes the database refuse a second writer of an
    immutable path on any node; the reservation only saves the loser's body on one node. The
    counter `hosted_allow_once_violation_total` stays as a tripwire for a port that ignores the
    mode (it must be 0).
12. **Group responses expose only a count of skipped members** (`X-Rustryx-Group-Skipped`),
    never member names: a caller learns that an answer may be incomplete, not which
    repository failed or which one it may not read.
13. **The policy hook is advisory, the port is the authority.** Alternative: enforce only in
    the pipeline. A check outside the transaction races with every other node, so a protected
    item could change; a check only inside it refuses after the body was received. Both, with the
    transaction deciding. A hook that cannot decide fails the request closed.
14. **Upstream validators live in a per node cache, not in the database**, because `Asset` has no
    column for them. Alternative: `If-Modified-Since` with our own cache time. It is unsafe
    (a change between the upstream's answer and our commit is invisible, and so is one inside the
    same second), so a node without the validators asks unconditionally and compares digests.
15. **A fill of a path without a copy is create-only; a refresh replaces.** The distinction is
    the presence of the expired copy in the plan of the fill, not a flag a format can set.
16. **Routing rules are cumulative across a group and a refusal wins.** Alternative: a member
    rule that overrides the group's. That would let a repository widen what the group's
    administrator blocked.
17. **Chunked sessions are tokens, not server state.** `repository.blob.handle` carries
    everything a node needs; the staged chunks are in the store. Alternative: a session table,
    which would be one more thing to expire, replicate and clean up.
18. **Counters are batched in the node, not written per download.** `DownloadBatcher` is a
    `ContentEvents` sink: the download path stays one event, and the interval, the cap and the
    final flush are the batcher's. A crash loses at most one interval, the accepted precision of a
    statistic.
19. **`RequestLabels` is a response extension.** The labels are decided where the request is
    classified and the cache relation is known (the engine), and read where the metric is
    recorded (the HTTP layer). Alternative: the HTTP layer re-derives them from the route
    and the headers, which every format shares one route for (`docs/slo.md` assumption A2).

## 12. Known gaps and follow-ups

Closed by core revision 2 and adopted here: gap 26 (create-only publish, `touch_fresh`), gap 27
(`RoutingRuleRepo`, `RuleRoutingGate`), gap 29 (resumable capability, `ChunkedSession`), the
typed refusals (`PolicyViolation`, `MutationError`), `UpstreamError`, the placement of blobs in a
group store. Gap 28 was rejected by core (no pending blob row): a leaked object is found by the
store sweep of `verify_blobs`.

For `rustryx-core` (not edited here):

* `Asset` has no place for the upstream's validators (`ETag`, `Last-Modified`). Revalidation works
  from a per node cache, so after a restart or on another node an expired copy is asked about
  without a condition (an identical body then no longer replaces the blob, but it is still
  downloaded). A field, or a small `metadata` column, would make it work everywhere.
* `ProxyConfig` has `ca_certificate_ids` but nothing in this crate resolves them: one shared
  HTTP client serves every repository, so per-repository CA bundles (SEC-27) are not supported.
* `ContentPolicy` has no implementer yet: `QuotaRepo::headroom`, immutability and holds are
  enforced by the transaction only, and the hook refuses early once they implement it.

In this crate:

* Presigned redirects (`BlobStore::presign_get`, design 7.d) are not decided here yet: they need
  the per store `presign` setting of the blob store definition, read per download, which wants
  a cache before it can sit on the hot path, plus the refusal rules of the design (never for
  `pub`, a manifest, a `HEAD`, a `304`, a proxy fill).
* The push event of a component (`LifecycleAdapter::completes_component`) is not attached to the
  publish: `UploadSpec` carries no events yet.
* A slow client holds an upstream concurrency permit while it is tee'd; tune
  `client_stall_timeout` and the per-repository cap.
* Followers of a cache miss wait for the whole fill (bounded by the upstream idle timeout).
* A refused fill (read only mode, an immutable expired copy) is retried on the next request,
  because the cache cannot be updated: every request pays the upstream round trip while the
  condition lasts.
* The blob store definition (for quotas) is read on every write; wrap the port in a cache
  if that shows up in profiles.
* A group member's `Policy` error is a 4xx and ends the search; groups only read, so it cannot
  happen today.
