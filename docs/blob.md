# Blob stores (`rustryx-blob`)

Implements the `BlobStore` and `BlobWriter` ports of `rustryx-core` (core revision 2) for a
local directory and for S3-compatible object storage, plus the hashing, registry, quota,
disk-floor and metrics helpers around them. Spec anchors: README 8.1 (async and hashing),
architecture 1.4 (streaming, multipart), SRE-08/13 (metrics), SRE-31/32/34 (quota, memory,
disk), BLOB-16 (presigned redirects), BLOB-19 (key listing for `verify_blobs`), Rust
engineering 2, 3, 6, 7. Not in this crate yet: Azure, group stores, resumable uploads, S3
encryption and pool options (wave 2, stream F; the registry refuses those kinds by name).

## Guarantees

| Guarantee | How it holds | Test |
|---|---|---|
| A blob is invisible until `commit` | local: temp file in `_staging/`, rename at commit. S3: nothing exists until `PutObject` or `CompleteMultipartUpload` | `must_not_make_the_blob_visible_before_commit`, `must_not_make_the_object_visible_before_commit` |
| Abort, drop and task cancellation leave nothing | `abort` removes or aborts; `Drop` schedules the same on a task tracker | `must_not_leave_a_partial_blob_after_{abort,drop}`, `..._when_the_writing_task_is_cancelled`, `..._when_commit_fails` and the S3 equivalents |
| Commit is atomic | `rename(2)` inside one file system; S3 completion is atomic | `commit_is_atomic_under_concurrent_writers_to_different_keys` |
| An acknowledged commit survives a crash | local: fsync of data before the rename, fsync of the directories after it | by construction (see "Local store") |
| Bodies are never held in memory | 64 KiB slices through a bounded buffer; S3 buffers one part | `must_not_buffer_whole_blob_in_memory` (512 MiB, RSS growth in the KiB range), SeaweedFS suite (160 MiB, about 26 MiB growth with 16 MiB parts) |
| A key cannot escape the root | strict allowlist, no `..`, no absolute path, reserved staging name, second check on the resolved path | `must_not_escape_the_root_with_hostile_keys` |
| An invalid key is never reported as a missing object | the typed `BlobError::InvalidKey` on every operation of both stores | `must_not_report_an_invalid_key_as_a_missing_blob`, `must_not_report_an_invalid_key_as_a_missing_object` |
| A local write below the free space floor is refused, reads, listings and deletes never are | `DiskGuard` is consulted by `begin` and by every `write`, nowhere else | `must_not_accept_a_write_below_the_free_space_floor`, `must_not_block_reads_listings_or_deletes_when_the_disk_is_full` |
| A listing never turns an outage, a missing root or an unordered backend into an empty page | errors surface; a missing local root is `Unavailable`; S3 names must arrive in ascending order | `must_not_report_a_missing_root_as_an_empty_store`, `a_listing_error_surfaces_instead_of_an_empty_page`, `must_not_page_through_a_backend_that_lists_out_of_order` |
| A listing never reports what `get` could not reach | names that are not a valid key of their own shard are skipped, symlinks are never followed | `must_not_report_files_that_are_not_blobs_of_their_shard`, `must_not_follow_symlinks_out_of_the_root`, `foreign_objects_are_not_blobs` |
| A cursor from a client is validated like a key | `key_of_location` accepts only `<b0>/<b1>/<valid key>` with the shard of that key | `must_not_follow_a_hostile_cursor` (local and S3) |
| A presigned URL lives at most 900 s and never reaches a log | clamp in two steps, `Secret` wrapper, no URL in any error | `must_not_sign_beyond_the_max_ttl`, `must_not_leak_a_presigned_url_through_debug_or_the_store` |
| A presigned URL cannot be changed by its holder | the response overrides are part of the SigV4 signature | SeaweedFS suite: tampered `response-content-disposition` is 403 |
| A key or path is never a metric label | closed label sets, pinned by a test over every series | `must_not_put_a_key_or_a_path_into_a_label` |
| Digests describe the stored bytes | SHA-1, SHA-256, MD5 computed from the very slices handed to the backend; a failed write poisons the writer | `must_not_commit_after_a_failed_write`, digest vector and chunk boundary tests |
| No std blocking calls, no hashing on the blocking pool | production code scanned in a test | `must_not_use_blocking_constructs_in_production_code` |
| The only thread this crate owns is the disk monitor | production code scanned for `std::thread` outside `disk.rs` | `must_not_start_threads_outside_the_disk_monitor` |

## Keys and layout

`BlobKey` is opaque text (`UUIDv7`). Both stores run the same validation before a key
becomes a path or an object name (`rustryx_blob::validate_key`):

- characters `[A-Za-z0-9._-]` and `/` as separator, at most 512 bytes, at most 200 bytes
  per component;
- no empty key, no leading or trailing `/`, no `//`, no `.` or `..` component, no NUL,
  no backslash, nothing non-ASCII;
- the first component must not be `_staging` (reserved).

Location of a committed blob: `<b0>/<b1>/<key>` where `b0` and `b1` are the first two
bytes of SHA-256 of the key in hex (65536 leaf directories). It is the same suffix on
S3: `<prefix>/<b0>/<b1>/<key>`. Nested keys such as `_uploads/docker/<id>` keep their
structure below the shard.

Keys that are prefixes of each other as paths (`a` and `a/b`) are a caller error only
when both land in the same shard; with opaque `UUIDv7` keys it does not arise.

## Local store

```
<root>/_staging/<uuidv7 hex>.part      in-flight writes, never served
<root>/<b0>/<b1>/<key>                 committed blobs
```

Write protocol: `begin` creates `_staging/<id>.part` with `O_EXCL`; `write` goes through
a 256 KiB `BufWriter`; `commit` flushes, `fsync`s the file, creates the shard directories
if needed, `rename`s onto the final path, then `fsync`s the final directory (and the
ancestors up to the root when directories were created). A failure of the directory
fsync is logged, not returned: the blob is already visible, failing the commit would
orphan a good object. Committing a key that already exists replaces it atomically; keys
are unique by contract, so this only matters for a retried commit.

Reads open the file and stream it through `ReaderStream` with a 64 KiB buffer, pulled by
the consumer. Ranges follow HTTP semantics: the end is clamped, a start past the last
byte is `RangeNotSatisfiable`.

`list_stale_staging(min_age)` returns `_staging/<id>.part` for staging files whose
modification time is older than `min_age`; `delete` accepts exactly those keys (a staging
key never serves content through `get` or `head`). Nothing deletes staging at startup:
on a shared RWX volume another node may be writing into it. A merely slow upload keeps
refreshing its modification time; one stalled for longer than `min_age` is
indistinguishable from an abandoned one, and deleting it makes its commit fail cleanly.

HA: the root must be a shared RWX volume (SRE-34). Directory permissions follow the
umask of the process; restrict the root to the service account.

### Free space floor (SRE-34)

A local store opened with `LocalOptions::with_disk_floor(probe, percent)` refuses writes
with `BlobError::InsufficientStorage` (507) when the write would leave less than `percent`
of the file system free. Reads, `head`, listings, `delete` and `health` are never refused:
deleting is how a full disk recovers. `LocalBlobStore::open(root)` has no floor and no
monitor (tests, tools); the registry builds production stores with the floor from the
definition (`min_free_percent`, default 10, 0 to 90, 0 switches the refusal off and keeps
the gauges).

How it holds:

- **`statvfs` through `rustix`, no `unsafe`.** `rustix::fs::statvfs` (MIT or Apache-2.0,
  already in the tree through `tempfile`) is the safe wrapper; `DiskUsage::free_bytes` is
  `f_bavail`, the space a non-root process can use, which is also what a node exporter
  alert compares. It compiles for Linux (checked for musl, `linux_raw` backend) and macOS (`libc` backend); only macOS ran.
- **The syscall runs on one dedicated OS thread per local store, never on the runtime.**
  `statvfs` has no async form and on an RWX volume (NFS, CephFS) it can hang as long as the
  mount does. The blocking pool is for hashing and parsing behind a semaphore (CONTRIBUTING
  rule 2), and a hung probe must not occupy it. The thread samples every 5 s, publishes the
  sample and the gauges, and ends when the store is dropped or shut down (it is never
  joined: joining would block on the hung mount). `open_with` waits at most 2 s for the
  first sample.
- **The write path reads a snapshot, not the disk.** `begin` checks `size_hint` against the
  sample (a write that announces a size across the floor is refused before a byte moves),
  and every 64 KiB slice is checked again, so an upload of unknown length stops when the
  disk crosses the floor. Bytes accepted since the sample are subtracted from the free
  space, which keeps a burst of uploads from running far past the floor between two samples
  (pessimistic: deletes are credited at the next sample).
- **No sample means no refusal.** A probe that fails keeps the last good sample (one log
  line per failure episode); one that never worked leaves the floor open. A guard that
  cannot see must not block a store whose reads and writes otherwise work. The
  `blob_store_free_bytes` series going stale is the signal.
- **A full disk that arrives anyway** (`ENOSPC`, `EDQUOT` from any write, flush, fsync,
  rename or `create_dir_all`) is `InsufficientStorage` too, not an opaque backend failure.

## S3 store

### Configuration (`BlobStoreDef::config` JSON)

Unknown fields are rejected, so a secret pasted into the JSON fails loudly. Error
messages name fields and limits, never values.

| Field | Default | Meaning |
|---|---|---|
| `bucket` | required | bucket name |
| `region` | `us-east-1` | signing region (SeaweedFS and Ceph accept any non-empty value) |
| `endpoint` | AWS | custom endpoint; `https` unless `allow_http` is true |
| `path_style` | true with an endpoint, false for AWS | `https://host/bucket/key` addressing |
| `allow_http` | false | permit a plain `http://` endpoint (trusted network only) |
| `prefix` | empty | object name prefix, no leading or trailing slash |
| `access_key_id_ref`, `secret_access_key_ref` | none | secret references, both or neither |
| `session_token_ref` | none | optional, needs the pair above |
| `part_size_mib` | 16 | 5 to 128; memory per upload is at most this |
| `max_concurrent_uploads` | 32 | 1 to 1024 |
| `presign` | `{"enabled": false, "ttl_secs": 300}` | presigned downloads, `ttl_secs` 1 to 900 |

Local store: `{"path": "/var/lib/rustryx/blobs", "min_free_percent": 10}` (absolute path
required; `min_free_percent` 0 to 90, default 10, 0 means no refusal).

With neither credential reference set, the client uses the ambient chain (web identity,
container credentials, instance metadata); `object_store` is never asked to read `AWS_*`
variables for static keys.

### Secrets

`BlobStoreRegistry` takes a `SecretResolver`. `EnvSecretResolver` resolves a reference
`NAME` from the file named by `NAME_FILE` (one trailing newline stripped), else from the
variable `NAME`. `MapSecretResolver` is for tests. `Secret` never prints its value in
`Debug`.

### Transport

TLS is rustls (`object_store` feature `aws`, `aws-lc-rs`), no `openssl-sys` anywhere in
the tree. Certificate verification uses the platform verifier, so minimal images need a
CA bundle (verify in the image build).

Timeouts: connect 3 s; idle (read) 60 s; metadata calls (head, delete, create and abort
multipart) 30 s; one part, a single-request upload and `CompleteMultipartUpload` 300 s.
The client's total-request timeout is disabled because it would also cut off long
streaming bodies; every call has an explicit deadline instead, and a deadline expiry is
`BlobError::Unavailable`.

Retries: the client retries 5xx, connection errors and (for safe requests) timeouts, at
most 3 times within 30 s, with exponential backoff and jitter. This is slightly wider
than "GET and HEAD only" because the client applies one policy to every request; retrying
a part upload (same part number) or `DELETE` is safe on S3. The store's own deadline
bounds the total.

### Upload path and memory

`begin` waits at most 10 s for an upload slot (a `Semaphore`, permit taken before any S3
call and released when the writer is committed, aborted or dropped) and then fails with
`Overloaded { retry_after: Some(5 s) }` (503 with `Retry-After` for the caller), so waiting
uploads never queue without bound.

The writer owns one buffer of at most `part_size` bytes. `write` does not return while a
full part is uploading, so a second part is never buffered. The buffer is allocated
lazily at the start of a part (never right after a flush, which would hold two parts at
once) and grown by hand, because `BytesMut::reserve` may double past the part size. When
the caller passes `size_hint >= part_size` the part is allocated once.

- An object of at most one part is a single `PutObject`. No multipart upload is created.
- A larger object creates the multipart upload when the first part fills up. Parts are
  uploaded one at a time. A full buffer is flushed only when more data arrives, so an
  object of exactly `k` parts has `k` parts and no empty tail (S3 rejects an empty part).
- `commit` flushes the remainder and completes the upload. If completing fails, the
  upload is aborted.
- Dropping or cancelling a writer aborts its multipart upload on a task tracker owned by
  the store; `BlobStore::shutdown` (or `BlobStoreRegistry::shutdown`) waits for those tasks.
- More than 10000 parts is an error. `size_hint` enlarges the part size only when the
  object would otherwise need more than 10000 parts (above 160 GB at 16 MiB).

Sizing: worst case memory for uploads is `max_concurrent_uploads x part_size`
(32 x 16 MiB = 512 MiB by default). The architecture target of 500 concurrent uploads
per node does not fit a 1 Gi memory limit with 16 MiB parts; choose smaller parts (5 MiB
is the S3 minimum) or a lower upload limit and rely on single-request uploads for small
artifacts, which only hold what they received.

### Required: bucket lifecycle rule for incomplete multipart uploads

A killed process cannot abort its uploads, and `object_store` 0.14 cannot list open
multipart uploads, so `capabilities().lists_staging` is false for S3, `list_stale_staging`
returns an empty list that means "cannot tell" (never "none"; the GC job checks the
capability before it trusts an empty answer) and the bucket must clean up by itself.
Without the rule, parts of every interrupted upload stay in the bucket and are billed
forever.

```json
{
  "Rules": [
    {
      "ID": "rustryx-abort-incomplete-multipart",
      "Status": "Enabled",
      "Filter": { "Prefix": "" },
      "AbortIncompleteMultipartUpload": { "DaysAfterInitiation": 1 }
    }
  ]
}
```

AWS: `aws s3api put-bucket-lifecycle-configuration --bucket <bucket> --lifecycle-configuration file://rule.json`.
Ceph RGW and SeaweedFS accept the same rule. Apply it. Do not rely on a server
default to abort an abandoned upload. Keep `DaysAfterInitiation` above the longest
legitimate upload.

### Permissions

`s3:PutObject`, `s3:GetObject`, `s3:DeleteObject`, `s3:AbortMultipartUpload` on the
objects and `s3:ListBucket` on the bucket. `ListBucket` is what makes S3 answer 404
instead of 403 for a missing key, and it is what `list_keys` uses; the health probe (a
`HEAD` on a key that never exists) needs it. A presigned URL carries the permissions of
the credentials that signed it, so those credentials need `s3:GetObject` and nothing more
for that purpose.

### Reads

`get` streams the body in chunks of at most 64 KiB (larger chunks from the HTTP client are
split without copying). A ranged read costs one extra `HEAD`, because the object store
reports an unsatisfiable range as an opaque error and the port promises
`RangeNotSatisfiable`. `head` returns the size and the last modification time
(`BlobMeta::modified`, which the staging sweep and `verify_blobs` use).

### Presigned downloads (BLOB-16)

`presign_get(key, ttl, options)` returns a `PresignedUrl` for a plain `GET`, or `Ok(None)`
when `presign.enabled` is false or the backend has no signer (the node then serves the
bytes itself; the local store also answers `Ok(None)`, as the port documents). It is
`Signer::signed_url_opts` of `object_store`, computed locally: no request is made.

- The lifetime is `min(ttl asked, presign.ttl_secs, 900 s)`, never below 1 s. Settings
  built in code with a longer TTL than the contract are capped too (two-step clamp, so a
  configured zero cannot make the minimum exceed the maximum).
- `response-content-type` and `response-content-disposition` are signed query parameters,
  so the holder cannot lift the `attachment` that the caller demands for active content
  (SEC-35). A value longer than 1024 bytes or with a control character is refused with
  `Unsupported` instead of being escaped and trusted.
- The URL is a bearer capability: it lives in a `Secret`, is never logged, appears in no
  error text, and the metrics carry no URL. `rustryx-observability` must still scrub
  `X-Amz-Signature` from request logs.
- No `HEAD` is made: the caller knows the asset is committed (design 7.d). A URL for a key
  that vanished answers 404 at the client.

### Listing keys (BLOB-19)

`list_keys(after, limit)` returns one page (at most 1000 keys, whatever `limit` asks for;
0 is raised to 1) in a stable order and an opaque cursor, `None` on the last page. The
cursor is the location (`<b0>/<b1>/<key>` without the store prefix) of the last key of the
page, so a key deleted between two pages does not break a walk. The order is the order of
the object names, not of the keys (the shard is a hash).

- Objects under the prefix that are not `<b0>/<b1>/<valid key>` with the shard of that key
  (a console upload, `_rustryx/health-probe`, a misfiled name) are skipped and counted in
  one log line: no `get`, `head` or `delete` could reach them.
- The names must arrive in ascending order. S3, SeaweedFS and Ceph list that way, but the
  `ObjectStore` trait does not promise it, and a walk over an unordered backend would repeat
  or skip keys without an error, so an out-of-order name is a `Backend` error.
- A listing error is an error, never an empty page. `verify_blobs` would read an empty
  page as "every row lost its object".
- The `_staging/` and `_uploads/` prefixes are not filtered here: nested keys such as
  `_uploads/docker/<id>` are committed objects, and the caller excludes the prefixes it
  handles elsewhere.

## Hashing

`BlobHasher` feeds SHA-1, SHA-256 and MD5 from the same slices of at most 64 KiB, in the
calling task (README 8.1). `HashingWriter<S: BlobSink>` is the shared writer: it slices,
hashes, forwards to the backend sink, and poisons itself after a failed write (later calls
return `WriterClosed`, `commit` discards the sink). After 1 MiB of hashed bytes without a
natural suspension point it yields to the scheduler, so a single `write` of a huge chunk
cannot hold a worker thread for more than a few milliseconds.

## Registry and quota

`BlobStoreRegistry::new(repo, secrets)`; `store_by_id(id)` loads the `BlobStoreDef`
through `BlobStoreRepo`, builds the store once (concurrent first calls share one build) and
caches it together with the definition. One instance per id matters: the S3 store owns the
upload semaphore and a local store owns its disk monitor. A failed build is not cached.

| Method | Use |
|---|---|
| `store_by_id(id)` | a plain store or a member; what `BlobStoreRegistry::store_by_id` of `rustryx-format-core` wraps |
| `store_for_repository(&Repository)` | the store that takes a new write: `repository.blob_store_id` (a group adapter replaces this in stream F) |
| `definition(id)` | the cached `BlobStoreDef` (`quota_bytes`), so the write path does not query the database per upload |
| `register(def)` | builds and replaces the cached store (a definition was just created or changed) |
| `invalidate(id)`, `invalidate_all()` | the bus told this node that a definition changed (`BlobStoreRepo::update` publishes it) or that it must reset |
| `with_disk_probe(probe)` | replaces `statvfs` for stores built afterwards |
| `shutdown()` | `BlobStore::shutdown` on every cached store |

The adapter that implements the `format-core` trait lives at wiring time and is one line per
method: `registry.store_by_id(id).await.map_err(BlobError::from)`. `RegistryError` maps to
`BlobError`: unknown definition or database down is `Unavailable` (a retry can help), a
root that cannot be opened keeps its own error, a wrong definition is `Backend` (500
class, detail in the logs). `Azure` and `Group` definitions fail with
`BuildError::UnsupportedKind` until stream F, so a definition written by a newer node is
never served as something it is not.

```rust
// before reading the body
check_quota(declared_len, repo.used_bytes_by_store(id).await?, def.quota_bytes)?;
// after commit, with the usage measured before this write
if let Err(over) = check_quota_after_write(commit.size_bytes, used_before, def.quota_bytes) {
    store.delete(&key).await?;
    return Err(over.into()); // DomainError::QuotaExceeded
}
```

The quota is a soft ceiling: two concurrent uploads can both pass the pre-check. Reaching
the quota exactly is allowed; without a declared size only an already full store is
rejected before the write.

## Capabilities

| Capability | Local | S3 |
|---|---|---|
| `lists_staging` | true | **false**: an empty `list_stale_staging` means "cannot tell", the bucket lifecycle rule is the contract |
| `lists_keys` | true | true |
| `can_presign` | false | true when a signer exists and `presign.enabled` |
| `resumable` | false | false (stream F) |

## Metrics

Emitted through the `metrics` facade; `rustryx-observability` owns the exporter. The names
and labels are the ones in `docs/slo.md` and `deploy/monitoring/rustryx-alerts.yaml`.

| Series | Type | Labels | Values |
|---|---|---|---|
| `blob_store_ops_total` | counter | `store`, `op`, `result` | `op`: `put`, `get`, `head`, `delete`, `list`, `presign`, `health`. `result`: `ok`, `not_found`, `rejected`, `overloaded`, `aborted`, `error` |
| `blob_store_bytes_total` | counter | `store`, `direction` | `write` (accepted from writers), `read` (handed to readers) |
| `blob_store_free_bytes` | gauge | `store` | local stores with a monitor; `f_bavail` times the fragment size |
| `blob_store_total_bytes` | gauge | `store` | same |

- `store` is the definition name: an administrator creates it, a handful per installation.
  No key, path, repository or user is ever a label value.
- Only `result="error"` feeds `RustryxBlobStoreErrors`. It is `Unavailable`, `Backend`,
  `InsufficientStorage` and any variant this build does not know. A missing object, a bad
  range or an invalid key is the caller's, a slot shortage is backpressure (`overloaded`).
- `put` is one whole write, counted once at the moment its fate is known: `begin` refused
  (slot, floor, bad key), the first failed `write`, `commit`, or `abort` (`aborted`). A
  writer dropped without any of them (a cancelled request) is not counted here; the format
  engine counts abandoned writers.
- A `get` counts at open; a failure in the middle of the body counts as another `get/error`.
- The gauges alert on `free / total < 0.15` and `< 0.05` per pod; the floor of 10 percent
  sits between them on purpose, so `RustryxDiskLow` fires before writes stop.
- The names are constants in `rustryx_blob::metrics`. `rustryx-observability/names.rs` does
  not list them yet (see Open issues).

## Error mapping

| Situation | `BlobError` |
|---|---|
| missing object, key below a regular file | `NotFound` (`head` returns `None`, `delete` succeeds) |
| range start at or past the end, `end < start` | `RangeNotSatisfiable` |
| S3 unreachable after retries, deadline expired | `Unavailable` |
| no upload slot within 10 s | `Overloaded { retry_after: Some(5 s) }` |
| invalid key, hostile list cursor, malformed staging id | `InvalidKey(reason)`, the reason names the rule and never the key |
| disk full (`ENOSPC`), quota (`EDQUOT`), below the free space floor | `InsufficientStorage` |
| presign with an oversized or control-character response header | `Unsupported` |
| permission or authentication failure on S3, other I/O errors | `Backend` with the source kept for logs |
| write or commit after a failed write | `WriterClosed` |

`list_keys` on a store that cannot list would be `Unsupported("list_keys")` (the port
default); both stores here can. `presign_get` is `Ok(None)` where there is no URL, not
`Unsupported`: the port documents `Ok(None)` for a store that has none and for a store with
presigning off, and a caller that forgot to probe `capabilities()` then serves the bytes
instead of failing a download.

## Tests

```
cargo test -p rustryx-blob                    # unit, local store, disk floor, listing, memory, rules; SeaweedFS when Docker answers
RUSTRYX_IT=1 cargo test -p rustryx-blob --test s3_seaweed -- --nocapture   # SeaweedFS required
RUSTRYX_IT=0 cargo test -p rustryx-blob       # never touch Docker
cargo llvm-cov -p rustryx-blob --summary-only
```

`RUSTRYX_SEAWEEDFS_IMAGE=name:tag` overrides the image. The default is
`chrislusf/seaweedfs:4.13`, the same tag `deploy/compose` pins.
`memory_streaming.rs` must keep exactly one test (RSS is per process).

The SeaweedFS suite also lists keys (paging, a foreign object under the prefix, a hostile
cursor) and fetches presigned URLs with a bare TCP client that holds no credentials: the
body and the signed overrides come back, a tampered override is 403, a key that is not
there is 404, a one second URL is 403 after it expires.

The unit tests read the metrics back through a recorder installed once per test process
(`metrics/capture.rs`): one series is emitted from the disk monitor thread, which a
thread-local recorder would never see, so every test gives its store a name of its own.

## Decisions

| # | Decision | Alternative | Why |
|---|---|---|---|
| B1 | Shard by SHA-256 of the key, two levels | literal key prefix | `UUIDv7` starts with a millisecond timestamp; all keys of ~49 days share eight hex digits and would pile into one directory or S3 prefix |
| B2 | Strict key allowlist plus reserved `_staging` first component | reject only `..`, absolute, NUL | Unicode normalisation and case folding can merge two keys on macOS and Windows; a staging name must never name a blob |
| B3 | Staging files are addressed as keys `_staging/<id>.part` | separate staging API | the GC job already deletes by key; `delete` understands exactly this shape |
| B4 | Low-level `MultipartStore`, multipart created lazily, single `PutObject` up to one part | `put_multipart` | explicit control of ids and aborts for `Drop`, one request for the common small artifact, and a fake backend for unit tests |
| B5 | Parts uploaded sequentially, bounded wait for a slot | pipelined parts, unbounded queue | memory per upload equals one part (architecture 1.4); a queue of waiting uploads is what README 8.1 forbids |
| B6 | `Drop` cleanup spawns on a store-owned `TaskTracker` | bare `tokio::spawn` or blocking removal in `Drop` | rule against fire-and-forget; shutdown can wait; no blocking call in `Drop` |
| B7 | `list_stale_staging` is empty on S3 and `lists_staging` is false | list through raw signed HTTP; return `Unsupported` | `object_store` has no listing of multipart uploads; the lifecycle rule is the contract; the port says an empty answer from such a store means "cannot tell", and `Unsupported` would fail a GC run that ignores capabilities |
| B8 | JSON holds secret references, `SecretResolver` supplies values | secrets in JSON | CONTRIBUTING rule 5; unknown fields rejected; error text never echoes values |
| B9 | One client retry policy (3 attempts, 30 s budget) plus per-call deadlines | no client retries | the client exposes one policy for all requests; safe for idempotent S3 calls |
| B10 | `HEAD` before a ranged `GET` on S3 | map the error text | exact `RangeNotSatisfiable` without parsing opaque errors |
| B11 | Yield after 1 MiB hashed | no yield, or `spawn_blocking` | README 8.1 forbids the blocking pool for hashing; yielding bounds worker occupancy |
| B12 | `BlobStore::shutdown` (core revision 2) replaces the side trait `CleanupDrain` and the inherent `drain_cleanup`; `BlobStoreRegistry::shutdown` calls it on every cached store | keep both | one hook, and it also stops the disk monitor |
| B13 | Disk capacity from `rustix::fs::statvfs` on a dedicated OS thread, behind a `DiskProbe` trait | `fs4` or `sysinfo` (a new package each); `spawn_blocking` | `rustix` is already in the tree and is the safe layer under `fs4`; the blocking pool is reserved for CPU work and a hung NFS mount must not occupy it; the trait is how tests move the disk |
| B14 | The guard checks a snapshot minus the bytes accepted since, at `begin` and on every slice | check the disk on every write; check only at `begin` | no request waits for the file system, a burst between two samples cannot run far past the floor, and an upload of unknown length still stops |
| B15 | `LocalBlobStore::open` has no floor; the registry applies `min_free_percent` (default 10) | floor on by default everywhere | tests and tools must not depend on how full the developer's disk is; production goes through the registry |
| B16 | `presign_get` returns `Ok(None)` for local and for presign off | `Unsupported` | the port documents `Ok(None)`; a missing capability probe then costs nothing |
| B17 | `list_keys` cursor is the key location, order is the order of names, page capped at 1000 | opaque token with a snapshot | stateless, survives deletion of the cursor key, one S3 request per page; ordering is verified, not assumed |
| B18 | `put` is counted once per write at its end; `result` has six values, only `error` alerts | per-chunk ops; `result` is ok or error | cardinality stays small and the alert does not fire on a missing object or on backpressure |
| B19 | The registry keeps the definition next to the store | read the definition on every write | the quota check runs on every upload (format-core notes this as a known limit); invalidation already exists |
| B20 | `BuildError::UnsupportedKind` for `Azure`, `Group` and kinds added later | build something close | a definition from a newer node must not be served as a different kind of store |

## Gaps in `rustryx-core`

Gaps 5 to 11 of `docs/core-gaps.md` are closed by core revision 2 and adopted here:
`InvalidKey` (5), `InsufficientStorage` (6), `Overloaded` (7), `BlobStore::shutdown` (8),
`BlobStoreRepo::update` (10, the registry is told through `invalidate`), `capabilities` for
"cannot list stale uploads" (11). Gap 9 was closed the other way: `BlobMeta` holds the size
and the modification time only, because S3 returns no digest on `HEAD`; the digests live in
the `blobs` row, and this crate sets `modified` where the backend knows it.

Still open or new, found while adapting:

1. No `BlobStore` method reports free and total bytes, so the `blob_store_stats` job of
   `rustryx-tasks` cannot read them through `Arc<dyn BlobStore>`. The gauges
   `blob_store_free_bytes` and `blob_store_total_bytes` carry the numbers for alerts; a
   `usage()` capability would carry them to the status API.
2. `PresignOptions` and the contract say nothing about how long a value may be or which
   characters are allowed; this crate refuses more than 1024 bytes and control characters
   with `Unsupported`. A typed variant would let the API tell the caller from a missing
   feature.
3. `KeyPage` has no "listing was truncated" field. A local store skips names that are not
   blobs of their shard and logs the count; the caller cannot see it.

## Using the crate from the wiring

1. `BlobStoreRegistry::new(repo, Arc::new(EnvSecretResolver))`, one per process.
2. Seed the default store from `config.blob` as a `BlobStoreDef`: for `local` the JSON is
   `{"path": local_path, "min_free_percent": local_min_free_percent}`.
3. On every `InvalidationEvent` for blob stores call `invalidate(id)`; on `Reset` call
   `invalidate_all()`.
4. Implement the `format-core` trait with `store_by_id` and `store_for_repository`, mapping
   errors with `BlobError::from`.
5. On graceful shutdown, after in-flight requests were drained: `registry.shutdown().await`.

## Open issues

- A commit whose response was lost on S3 can leave a complete object with no `blobs`
  row. The GC is database driven (`BlobRepo::list_unreferenced`), so it never sees such an
  object; `verify_blobs` (design 7.d) finds it with `list_keys`, which exists now.
- The free space floor is checked against a 5 s snapshot; with a probe that never worked
  the floor stays open. Both are by design (see "Free space floor"), and both are visible
  as a stale `blob_store_free_bytes`.
- The metric names are constants in `rustryx_blob::metrics`; `rustryx-observability`
  `names.rs` and `deploy/monitoring/check_assets.py` still list the four series as "not yet
  exported". Move them to the exported table of `docs/slo.md` and add the constants to
  `names.rs` (the checker fails while a series is on both lists).
- `rustryx-config` has `blob.local_min_free_percent`, but nothing yet copies it into the
  JSON of the default `BlobStoreDef` (wiring step 2 above).
- S3 request checksums are off (`x-amz-checksum-*`): enabling them makes the client hash
  every part on the async worker. Integrity relies on TLS and the ETag check of S3.
- S3 encryption, connection pool size and assume-role options, Azure, group stores and
  `ResumableBlobStore` are stream F and are not here.
- The disk monitor is one OS thread per local store (few per process). A deployment with
  hundreds of local stores would want one shared thread.
- The Linux runtime of the `statvfs` wrapper was not exercised (the dev machine is macOS);
  it compiles for `x86_64-unknown-linux-musl` and uses the same `rustix` call as the macOS
  test that passes.
- The `aws-lc-rs` provider needs a C toolchain for the musl static build, same as the
  other crates that already use it.
