# Blob stores (`rustryx-blob`)

Implements the `BlobStore` and `BlobWriter` ports of `rustryx-core` for a local
directory and for S3-compatible object storage, plus the hashing, registry and quota
helpers around them. Spec anchors: README 8.1 (async and hashing), architecture 1.4
(streaming, multipart), SRE-31/32/34 (quota, memory, disk), Rust engineering 2, 3, 6, 7.

## Guarantees

| Guarantee | How it holds | Test |
|---|---|---|
| A blob is invisible until `commit` | local: temp file in `_staging/`, rename at commit. S3: nothing exists until `PutObject` or `CompleteMultipartUpload` | `must_not_make_the_blob_visible_before_commit`, `must_not_make_the_object_visible_before_commit` |
| Abort, drop and task cancellation leave nothing | `abort` removes or aborts; `Drop` schedules the same on a task tracker | `must_not_leave_a_partial_blob_after_{abort,drop}`, `..._when_the_writing_task_is_cancelled`, `..._when_commit_fails` and the S3 equivalents |
| Commit is atomic | `rename(2)` inside one file system; S3 completion is atomic | `commit_is_atomic_under_concurrent_writers_to_different_keys` |
| An acknowledged commit survives a crash | local: fsync of data before the rename, fsync of the directories after it | by construction (see "Local store") |
| Bodies are never held in memory | 64 KiB slices through a bounded buffer; S3 buffers one part | `must_not_buffer_whole_blob_in_memory` (512 MiB, RSS growth in the KiB range), MinIO suite (160 MiB, about 26 MiB growth with 16 MiB parts) |
| A key cannot escape the root | strict allowlist, no `..`, no absolute path, reserved staging name, second check on the resolved path | `must_not_escape_the_root_with_hostile_keys` |
| Digests describe the stored bytes | SHA-1, SHA-256, MD5 computed from the very slices handed to the backend; a failed write poisons the writer | `must_not_commit_after_a_failed_write`, digest vector and chunk boundary tests |
| No std blocking calls, no hashing on the blocking pool | production code scanned in a test | `must_not_use_blocking_constructs_in_production_code` |

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

Not implemented here: rejecting writes when less than 10% of the disk is free (SRE-34).
It needs `statvfs`, and `unsafe` is forbidden in this workspace; use a safe wrapper crate
in the composition root or a sidecar metric.

## S3 store

### Configuration (`BlobStoreDef::config` JSON)

Unknown fields are rejected, so a secret pasted into the JSON fails loudly. Error
messages name fields and limits, never values.

| Field | Default | Meaning |
|---|---|---|
| `bucket` | required | bucket name |
| `region` | `us-east-1` | signing region (`MinIO` and Ceph accept any non-empty value) |
| `endpoint` | AWS | custom endpoint; `https` unless `allow_http` is true |
| `path_style` | true with an endpoint, false for AWS | `https://host/bucket/key` addressing |
| `allow_http` | false | permit a plain `http://` endpoint (trusted network only) |
| `prefix` | empty | object name prefix, no leading or trailing slash |
| `access_key_id_ref`, `secret_access_key_ref` | none | secret references, both or neither |
| `session_token_ref` | none | optional, needs the pair above |
| `part_size_mib` | 16 | 5 to 128; memory per upload is at most this |
| `max_concurrent_uploads` | 32 | 1 to 1024 |

Local store: `{"path": "/var/lib/rustryx/blobs"}` (absolute path required).

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
`Unavailable` (503 for the caller), so waiting uploads never queue without bound.

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
  the store; `drain_cleanup` (or `BlobStoreRegistry::shutdown`) waits for those tasks.
- More than 10000 parts is an error. `size_hint` enlarges the part size only when the
  object would otherwise need more than 10000 parts (above 160 GB at 16 MiB).

Sizing: worst case memory for uploads is `max_concurrent_uploads x part_size`
(32 x 16 MiB = 512 MiB by default). The architecture target of 500 concurrent uploads
per node does not fit a 1 Gi memory limit with 16 MiB parts; choose smaller parts (5 MiB
is the S3 minimum) or a lower upload limit and rely on single-request uploads for small
artifacts, which only hold what they received.

### Required: bucket lifecycle rule for incomplete multipart uploads

A killed process cannot abort its uploads, and `object_store` 0.14 cannot list open
multipart uploads, so `list_stale_staging` returns an empty list for S3 and the bucket
must clean up by itself. Without the rule, parts of every interrupted upload stay in the
bucket and are billed forever.

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
Ceph RGW supports the same rule. `MinIO` also removes stale uploads on its own
(configurable expiry, 24 h by default in current releases; check your version) and
accepts the rule. Keep `DaysAfterInitiation` above the longest legitimate upload.

### Permissions

`s3:PutObject`, `s3:GetObject`, `s3:DeleteObject`, `s3:AbortMultipartUpload` on the
objects and `s3:ListBucket` on the bucket. `ListBucket` is what makes S3 answer 404
instead of 403 for a missing key; the health probe (a `HEAD` on a key that never exists)
needs it.

### Reads

`get` streams the body in chunks of at most 64 KiB (larger chunks from the HTTP client are
split without copying). A ranged read costs one extra `HEAD`, because the object store
reports an unsatisfiable range as an opaque error and the port promises
`RangeNotSatisfiable`.

## Hashing

`BlobHasher` feeds SHA-1, SHA-256 and MD5 from the same slices of at most 64 KiB, in the
calling task (README 8.1). `HashingWriter<S: BlobSink>` is the shared writer: it slices,
hashes, forwards to the backend sink, and poisons itself after a failed write (later calls
return `WriterClosed`, `commit` discards the sink). After 1 MiB of hashed bytes without a
natural suspension point it yields to the scheduler, so a single `write` of a huge chunk
cannot hold a worker thread for more than a few milliseconds.

## Registry and quota

`BlobStoreRegistry::new(repo, secrets)`; `store_for(id)` loads the `BlobStoreDef`, builds
the store once (concurrent first calls share one build) and caches it. One instance per
id matters: the S3 store owns the upload semaphore. A failed build is not cached.
`register(def)` builds and replaces, `invalidate(id)` forgets, `shutdown()` drains cleanup.

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

## Error mapping

| Situation | `BlobError` |
|---|---|
| missing object, key below a regular file | `NotFound` (`head` returns `None`, `delete` succeeds) |
| range start at or past the end, `end < start` | `RangeNotSatisfiable` |
| S3 unreachable after retries, deadline expired, no upload slot | `Unavailable` |
| invalid key | `Backend { context: "invalid blob key", source: KeyError }` |
| permission or authentication failure on S3, I/O errors | `Backend` with the source kept for logs |
| write or commit after a failed write | `WriterClosed` |

## Tests

```
cargo test -p rustryx-blob                    # unit, local store, memory, rules; MinIO when Docker answers
RUSTRYX_IT=1 cargo test -p rustryx-blob --test s3_minio -- --nocapture   # MinIO required
RUSTRYX_IT=0 cargo test -p rustryx-blob       # never touch Docker
cargo llvm-cov -p rustryx-blob --summary-only
```

`RUSTRYX_MINIO_IMAGE=name:tag` overrides the image. The `minio/minio` repository no longer
exists on Docker Hub; the default falls back to `cgr.dev/chainguard/minio:latest`, the
same server. `memory_streaming.rs` must keep exactly one test (RSS is per process).

## Decisions

| # | Decision | Alternative | Why |
|---|---|---|---|
| B1 | Shard by SHA-256 of the key, two levels | literal key prefix | `UUIDv7` starts with a millisecond timestamp; all keys of ~49 days share eight hex digits and would pile into one directory or S3 prefix |
| B2 | Strict key allowlist plus reserved `_staging` first component | reject only `..`, absolute, NUL | Unicode normalisation and case folding can merge two keys on macOS and Windows; a staging name must never name a blob |
| B3 | Staging files are addressed as keys `_staging/<id>.part` | separate staging API | the GC job already deletes by key; `delete` understands exactly this shape |
| B4 | Low-level `MultipartStore`, multipart created lazily, single `PutObject` up to one part | `put_multipart` | explicit control of ids and aborts for `Drop`, one request for the common small artifact, and a fake backend for unit tests |
| B5 | Parts uploaded sequentially, bounded wait for a slot | pipelined parts, unbounded queue | memory per upload equals one part (architecture 1.4); a queue of waiting uploads is what README 8.1 forbids |
| B6 | `Drop` cleanup spawns on a store-owned `TaskTracker` | bare `tokio::spawn` or blocking removal in `Drop` | rule against fire-and-forget; shutdown can wait; no blocking call in `Drop` |
| B7 | `list_stale_staging` is empty on S3 | list through raw signed HTTP | `object_store` has no listing of multipart uploads; the lifecycle rule is the contract |
| B8 | JSON holds secret references, `SecretResolver` supplies values | secrets in JSON | CONTRIBUTING rule 5; unknown fields rejected; error text never echoes values |
| B9 | One client retry policy (3 attempts, 30 s budget) plus per-call deadlines | no client retries | the client exposes one policy for all requests; safe for idempotent S3 calls |
| B10 | `HEAD` before a ranged `GET` on S3 | map the error text | exact `RangeNotSatisfiable` without parsing opaque errors |
| B11 | Yield after 1 MiB hashed | no yield, or `spawn_blocking` | README 8.1 forbids the blocking pool for hashing; yielding bounds worker occupancy |
| B12 | `CleanupDrain` trait and `BlobStoreRegistry::shutdown` | change the core port | the core crate is out of scope; shutdown still needs a hook |

## Gaps in `rustryx-core` (reported, not edited)

1. `BlobError` has no `InvalidKey`. An invalid key is a caller bug and must not read as
   `NotFound`, so it travels as `Backend` with a `KeyError` source; the HTTP layer should
   downcast it (400 or 500) rather than treat every `Backend` as 500.
2. No variant for "disk full" or "quota": a full local disk surfaces as `Backend`.
   SRE-34 asks for 507. A `BlobError::InsufficientStorage` would let the API map it.
3. No `Overloaded` (503 with `Retry-After`) variant; running out of upload slots uses
   `Unavailable`, which maps to 503 but cannot carry the retry hint.
4. `BlobStore` has no shutdown or drain hook; `CleanupDrain` lives in this crate.
5. `BlobStore::list_stale_staging` cannot express "this backend cannot list"; S3 returns
   an empty list. The doc of the port should say which keys `delete` accepts afterwards.
6. `BlobMeta` is documented as "size and digests" but holds only the size.
7. `BlobStoreRepo` has no `update`; changing a definition means delete and create, and the
   registry cache must be told (`invalidate`).

## Open issues

- A commit whose response was lost on S3 can leave a complete object with no `blobs`
  row. The GC is database driven (`BlobRepo::list_unreferenced`), so it never sees such an
  object; a periodic listing reconciliation job is needed.
- Disk-free rejection for the local store (SRE-34) is not implemented (see above).
- No metrics yet (upload slots in use, bytes, cleanup failures); they belong to the
  observability crate's registry once it exists.
- S3 request checksums are off (`x-amz-checksum-*`): enabling them makes the client hash
  every part on the async worker. Integrity relies on TLS and the ETag check of S3.
- The `aws-lc-rs` provider needs a C toolchain for the musl static build, same as the
  other crates that already use it.
