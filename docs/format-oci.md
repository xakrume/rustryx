# rustryx-format-oci

The OCI Distribution handler for the `docker` and `oci` repository labels. One `OciHandler` implements `FormatHandler` of `rustryx-format-core` (`docs/format-core.md`). Helm charts pushed with `helm push` (an `oci://` reference) are ordinary OCI manifests and blobs; this crate has no chart index and no second protocol. Hosted upload and download, chunked uploads, the proxy cache, group fan-out, the SSRF guard and the security invariants come from format-core. This crate is the registry protocol and the token service.

The handler is not registered in `rustryx-server`. Section 6 is what that crate has to mount.

Public protocol sources (no code or text of any other artifact manager was used):

* distribution: <https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md>
* image manifest and index: <https://github.com/opencontainers/image-spec/tree/v1.1.0>
* token authentication: <https://distribution.github.io/distribution/spec/auth/token/>
* Docker schema 2 manifest: <https://github.com/distribution/distribution/blob/main/docs/content/spec/manifest-v2-2.md>

Contents: 1 protocol surface, 2 behaviour, 3 configuration, 4 limits, 5 negative requirements and their tests, 6 what the server must mount, 7 gaps, 8 running the tests.

## 1. Protocol surface

The HTTP layer takes the Rustryx repository from the first segment under `/v2/` or from `/repository/<name>/`, then hands the engine `head.path` starting at `v2/`. The same image is reached both ways (`docs/testserver.md` section 5).

| Request, below the Rustryx repository | Status |
|---|---|
| `GET` and `HEAD` of the empty path (`/v2/` of that repository) | 200 `{}` after authorization |
| `GET` and `HEAD <name>/manifests/<tag or digest>` | hosted, proxy, group |
| `PUT` and `DELETE <name>/manifests/<reference>` | hosted only (the engine answers 405 on proxy and group before the body is read) |
| `GET` and `HEAD <name>/blobs/<digest>` | hosted, proxy, group |
| `DELETE <name>/blobs/<digest>` | hosted |
| `POST <name>/blobs/uploads/` with `?digest=` | hosted monolithic upload |
| `POST` with `?mount=<digest>&from=<repo>/<image>` | hosted cross-repository mount, or a new session when the mount is not done |
| `POST` without those parameters, then `PATCH`, `PUT ?digest=`, `GET`, `DELETE` on `<name>/blobs/uploads/<session>` | hosted chunked upload, when the blob store is resumable |
| `GET` and `HEAD <name>/tags/list` | hosted from stored tags; proxy live; group is the union |
| `GET` and `HEAD <name>/referrers/<digest>` | hosted from referrer links and the tag schema; proxy live; group merges |
| `GET` and `HEAD _catalog` | images of this repository the caller may browse |
| `GET /v2/_catalog`, `GET /v2/`, `GET` and `POST /v2/token` | not repository routes; the server mounts them (section 6) |
| Docker schema 1 manifests | refused (`MANIFEST_INVALID`) |

Blob addresses are `sha256` only. Manifests may be addressed by `sha256` or `sha512`.

## 2. Behaviour

### 2.1 Classification

`classify` decodes the path once (`NormalizedPath`, at most 512 bytes, trailing slash allowed) and parses an `OciRoute` from the end, so an image path may itself contain `blobs`, `manifests`, `tags` or `referrers`. The canonical path of that route is what authorization, routing rules and storage see. `handle` parses that text again and never the raw path.

`GET` and `HEAD` of a manifest or blob are `Read`. `PUT` of a manifest and every upload method are `Write`. `DELETE` is `Delete`. Tag lists and referrers are `Read` and index documents. `_catalog` is `Browse` and an index document. A wrong method is `405` with `Allow`. An unknown path is `404`. A bad name, tag, digest or session id is `400` with `NAME_INVALID`, `MANIFEST_INVALID`, `DIGEST_INVALID` or `BLOB_UPLOAD_INVALID`.

The coordinate is the image path, its last component, and the tag or digest when the route has one.

### 2.2 Where bytes live

There is no tag table. State is assets and components:

| What | Asset path | Component |
|---|---|---|
| manifest | `<image>/manifests/<digest>` | `name` = image, `version` = digest |
| tag | `<image>/manifests/<tag>`, the same blob as the manifest | that manifest's component |
| blob | `<image>/blobs/<digest>` | none |
| referrer link | `<image>/referrers/<subject>/<referrer>`, the referrer manifest's blob | the referrer's component |

A tag move is one `AssetRepo::link`. Deleting a manifest deletes its component, so the manifest asset, tags that point at it, and referrer links it owns go together.

### 2.3 Hosted manifests and blobs

A manifest `PUT` reads the body under `manifest_max_bytes` (default 4 MiB; over it, `413`). The document must be schema version 2 and one of: OCI image manifest, OCI index, Docker schema 2, Docker manifest list. `Content-Type` and `mediaType` must agree when both are present. At most `max_descriptors` descriptors are accepted.

When the URL names a digest, it must be the digest of those exact bytes (`DIGEST_INVALID` otherwise, and nothing is stored: `must_not_store_a_manifest_whose_url_digest_does_not_match`). Referenced blobs and child manifests must already exist in the image and have the size the descriptor states (`MANIFEST_BLOB_UNKNOWN`, or `MANIFEST_INVALID` when the size differs). A descriptor with `urls` is a foreign layer and is not required. A `subject` may be absent from the registry. The manifest is stored under its digest. A tag in the URL is linked to that blob. A subject records a referrer link and `OCI-Subject`. Pushing the same digest again does not write it again, so `allow_once` still accepts a repeat.

A blob `POST ?digest=sha256:...` is one hosted upload with that checksum. A mismatch is `400 DIGEST_INVALID` and the object is not kept (`must_not_store_a_blob_whose_digest_does_not_match`). A blob that is already stored for that digest answers `201` without reading a new copy.

`POST ?mount=<digest>&from=<rustryx-repo>/<image>` links the blob with `AssetRepo::link` only when the caller may `Read` the source asset, the source is a hosted `docker` or `oci` repository, the digest is `sha256`, and the two repositories can share the blob. Every other outcome (unknown source, no permission, missing blob, different blob stores, a bad digest) is the same `202` session a normal upload would start. The client is not told which reason it was (`must_not_mount_a_blob_from_a_repository_the_caller_cannot_read`). A caller who can read the source gets `201` and no byte copy. If the target already has the digest, the answer is `201` without a link.

Chunked uploads use format-core sessions (`repository-uuid.blob-uuid.store-handle`). The session is not a secret: anyone who may write the repository can continue it, and a token of another repository is `BLOB_UPLOAD_UNKNOWN`. A store that is not resumable answers `501` on `POST` without `digest` and tells the client to use a monolithic `POST`. `PATCH` appends at the `Content-Range` start, or at the staged length when no range is sent. A range that does not match `Content-Length` is `SIZE_INVALID`. An offset that is not the staged length is `416` with `Range` set to the real length. A declared length over the blob cap or the quota is `413 SIZE_INVALID` (or `507 DENIED` for a quota) before the chunk is staged (`must_not_accept_a_chunk_that_exceeds_the_blob_limit`). `PUT ?digest=` closes the upload and checks the digest the same way as a monolithic upload. `DELETE` aborts the session. `OCI-Chunk-Min-Length` is set on `202` only when `OciConfig::chunk_min_length` is `Some`.

`GET` and `HEAD` of a blob or manifest go through the hosted download (ranges, validators). The response adds `Docker-Content-Digest`. A manifest whose stored type is outside the client's `Accept` is `MANIFEST_UNKNOWN`.

### 2.4 Listings

`tags/list` reads assets under `<image>/manifests/` up to `tag_scan_cap`. A path whose remainder is a tag (no `:` and no `/`) is a tag. Over the cap the answer is `503`, not a short list. `n` and `last` page it, with a `Link` rel=`next` when more remain. An image with no manifest component is `NAME_UNKNOWN`.

`GET /v2/<repo>/_catalog` lists image names as `<repo>/<image>` that the caller may `Browse`. `GET /v2/_catalog` is `CatalogService`: every online `docker` and `oci` repository the caller may see, groups expanded to the union of members (depth 4, 64 members), then filtered again by browse on the group. An anonymous principal sees a repository only when `allow_anonymous` is set. A repository the caller cannot see is omitted and does not change the shape of the answer. Over `catalog_scan_cap`, or when authorization cannot answer, the status is `503`.

Referrers are an OCI index. Links stored at push are read, and a tag named by the referrers tag schema (`<algorithm>-<hex>`, hex cut to 64 characters) is included when it holds an index. `artifactType` filters the list and sets `OCI-Filters-Applied: artifactType`. The page size is `referrers_page_size`. More than `referrers_scan_cap` links is `503`.

### 2.5 Proxy

The upstream name is the image path, unless `namespace_allowlist` is non-empty and the image matches none of its globs: then the name does not exist (`404 NAME_UNKNOWN`) and the upstream is not called (`must_not_pull_through_a_proxy_outside_the_namespace_allow_list`). A glob that does not compile matches nothing, so a broken list does not become an open proxy. Single-segment names are prefixed with `library/` when the remote host is a Docker Hub host or `format_config.library_namespace` is true.

Manifests by digest are cached without a TTL and checked against the URL digest before they are stored. Manifests by tag use the metadata TTL. The client's `Accept` is narrowed to the four manifest types this registry stores. Tag lists and referrers are not cached; they are read live, with the client's `n` and `last`, and bounded. An upstream `401` or `403` is not forwarded.

Upstream bearer tokens are cached per repository and scope. A refused bearer is forgotten and tried once more. When the repository has stored Basic credentials and the upstream wants a bearer token, the proxy pipeline cannot carry both headers, so that pull is not cached and goes through `UpstreamExecutor` with the bearer only (SSRF, breaker, timeouts, retries and redirect checks still apply). A blob in that path is checked while it streams; a mismatch fails the transfer. Ranges are not forwarded there.

### 2.6 Groups

Manifests and blobs are the first member that has them. `tags/list` and `_catalog` are the sorted union of member documents, paged again. Referrers are the union of descriptors, first member wins per digest, then filtered and capped. A member document that is not usable JSON is skipped. If none of the members returned a usable listing, the merge is `422`.

`OciClaimer` claims the image name, so a hosted member that holds the name stops later non-hosted members from being asked.

### 2.7 Anonymous pull

The engine refuses an anonymous principal when `allow_anonymous` is false, before the handler reads a body, even if RBAC would allow the call. The rendered error is `401 UNAUTHORIZED` and does not contain the blob (`must_not_serve_an_anonymous_pull_without_the_repository_flag`). With the flag on, the same principal is authorized by RBAC and the blob is served. The global anonymous switch is not in this crate: the HTTP layer only builds an anonymous principal when that switch is on.

`GET /v2/` without a credential is still `401` with the bearer challenge, including when anonymous pull is on. The client then asks `/v2/token` for an anonymous token. Whether that token contains `pull` is RBAC's answer for that principal.

### 2.8 Tokens

`OciTokenService::issue` checks each requested action with `RbacChecker::check`. An action RBAC denies is left out. A repository that does not exist, or is not `docker` or `oci`, yields an empty grant, the same shape as a deny, so the endpoint is not a repository probe. `push` and `delete` are granted only on hosted repositories. A failing authorization backend is `503`, not an empty grant and not access. The lifetime is `TokenServiceConfig::ttl` (default 5 minutes), not a request field. A wrong `service` is `400`. A malformed scope is `400`. Having no access is still `200` and a token whose `access` omits the denied actions.

`token_response` returns `token`, `access_token`, `expires_in` and `issued_at`, with `Cache-Control: no-store`. The `oauth` flag adds `scope` for what was actually granted. This crate does not read `grant_type`; the server decides which POST bodies it accepts.

`HmacTokenCodec` signs HS256 over one key of at least 32 bytes. The header must say `HS256`. `none` and any other algorithm are refused, and `jku`, `x5u`, `kid` and `jwk` are not followed. The signature is checked before the payload is parsed. `exp`, `nbf` and `iat` are checked with 5 seconds of leeway. Audience and issuer must match. A token longer than 8 KiB is refused before decoding. The key is not included in `Debug`.

`bearer_challenge` is `Bearer realm="<base>/v2/token",service="<service>"` plus optional `scope` and `error`. `challenge_scope` asks `pull` for `GET` and `HEAD`, `pull,delete` for `DELETE`, and `pull,push` otherwise.

### 2.9 Errors

Bodies are `{"errors":[{"code","message","detail"}]}` with `Content-Type: application/json` and `Docker-Distribution-API-Version: registry/2.0`. Codes are the distribution set plus `UNAVAILABLE` and `UNKNOWN`. `render_error` maps engine errors by status: `401 UNAUTHORIZED`, `403` and `409` and `507` as `DENIED`, `404 NAME_UNKNOWN` (a route then relabels a missing manifest, blob or upload), `413 SIZE_INVALID`, `429 TOOMANYREQUESTS`. A message that starts with `[CODE] ` (from `classify`) keeps that code. A `5xx` from the handler is returned to the engine so it is logged once and a group does not forward it. Client text does not include SQL, paths, upstream addresses or signing keys (`must_not_leak_internal_text_in_a_body`).

## 3. Configuration

`OciConfig` (defaults in parentheses): `manifest_max_bytes` (4 MiB), `max_descriptors` (8192), `tag_scan_cap` (100000), `catalog_scan_cap` (100000), `referrers_page_size` (500), `referrers_scan_cap` (5000), `chunk_min_length` (unset), `upstream_token_capacity` (4096), `upstream_probe_ttl` (300 s), `upstream_token_max_ttl` (3600 s), `upstream_token_response_max_bytes` (64 KiB).

`RepositoryOptions::from_format_config` reads `library_namespace` as a boolean. Any other key, or a value that is not a boolean, is ignored. `None` means: use the `library/` prefix only for `registry-1.docker.io`, `index.docker.io`, `registry.docker.io` and `docker.io`.

`TokenServiceConfig`: `service` and `issuer` default to `rustryx`, `ttl` to 300 seconds.

The blob size cap and the quota are `Limits` and the repository, from format-core, not fields of `OciConfig`.

## 4. Limits

| Limit | Value | Where |
|---|---|---|
| manifest body | `manifest_max_bytes`, default 4 MiB | `413` before the rest of the document is kept |
| descriptors in one manifest | `max_descriptors`, default 8192 | `MANIFEST_INVALID` |
| tag scan | `tag_scan_cap` | `503` rather than a short list |
| catalog scan | `catalog_scan_cap` | `503` |
| referrers | `referrers_scan_cap` read, `referrers_page_size` returned | `503` when the scan cap is passed |
| group nesting | 4 deep, 64 members | deeper groups contribute no images |
| decoded registry path | 512 bytes | `NAME_INVALID` |
| image name | 255 bytes, OCI grammar, no uppercase | `NAME_INVALID` |
| upload session id | 1 to 256 of `A-Z a-z 0-9 . _ ~ -` | `BLOB_UPLOAD_INVALID` |
| registry token | 8 KiB | refused before decode |
| signing key | at least 32 bytes | refused at construction |
| blob bytes | `Limits::max_blob_bytes` and quotas | `413` or `507` |

## 5. Negative requirements and their tests

| Requirement | Test |
|---|---|
| A blob whose bytes do not match the declared `sha256` is not stored | `negative::must_not_store_a_blob_whose_digest_does_not_match` |
| A manifest whose bytes do not match the digest in the URL is not stored, and the check runs before missing layers are reported | `negative::must_not_store_a_manifest_whose_url_digest_does_not_match` |
| A mount from a repository the caller cannot read does not link the blob; the answer is the same `202` as a new upload. A caller who can read the source does get the link | `negative::must_not_mount_a_blob_from_a_repository_the_caller_cannot_read` |
| A chunk whose `Content-Length` is over the blob cap is `413 SIZE_INVALID` and stages nothing | `negative::must_not_accept_a_chunk_that_exceeds_the_blob_limit` |
| Anonymous pull is `401` when `allow_anonymous` is off, even if RBAC allows, and the body is not the blob. With the flag on, the same call returns the blob | `negative::must_not_serve_an_anonymous_pull_without_the_repository_flag` |
| A proxy name outside `namespace_allowlist`, and any name when every pattern fails to compile, is `NAME_UNKNOWN` and does not call the upstream | `negative::must_not_pull_through_a_proxy_outside_the_namespace_allow_list` |
| An expired, not-yet-valid, tampered, cross-signed, `none`, wrong-audience or wrong-issuer token is refused | `token::codec::tests::must_not_accept_*` |
| Error bodies do not echo internal text | `error::tests::must_not_leak_internal_text_in_a_body` |
| Names, digests, tags, routes, scopes, session ids, media types, manifests, link headers and bearer challenges do not panic on arbitrary input | proptests in `parse::*` and `proxy::challenge` |

`OciTokenService` leaving out denied actions, refusing `push` and `delete` on a proxy or a group, and failing closed when RBAC is unavailable, is implemented in `token/service.rs` and is not covered by a test in this crate. The codec tests cover the signed token, not that intersection.

## 6. What the server must mount

Register the handler once:

```rust
registry.register(Arc::new(OciHandler::new(OciConfig::default())))?;
```

`formats()` is `[RepoFormat::Docker, RepoFormat::Oci]`. A second registration of either label is refused by `FormatRegistry`.

These three are not `FormatEngine::serve` routes. The API crate mounts them, the same way `docs/testserver.md` section 5 describes the harness:

| Route | Function | Notes |
|---|---|---|
| `GET /v2/` and `GET /v2` without a valid credential | `ping_challenge(base_url, service)` | `401`, bearer `realm` pointing at `<base>/v2/token`, `Docker-Distribution-API-Version: registry/2.0`. Always, including when anonymous pull is on |
| the same with a valid registry token, API token or Basic credential | `ping_ok()` | `200` `{}`. `docker login` requires this |
| `GET /v2/token` | `TokenRequest::from_urlencoded` on the query, then `token_response(..., oauth = false)` | Basic or anonymous principal, supplied by the API. Wrong credentials are the API's `401`, not this function |
| `POST /v2/token` | same, `oauth = true`, so the JSON includes `scope` | this crate does not look at `grant_type`; the server rejects other grants |
| `GET /v2/_catalog` | `CatalogService` over the repository, component and RBAC ports, then `catalog_response` | spans repositories, so it cannot go through `serve` |

`OciTokenService::verify` is what turns `Authorization: Bearer` on `/v2/` into claims. The engine still authorizes the repository request through RBAC, so a token does not widen what the principal may do.

Repository traffic stays on the engine: `/v2/<repository>/<image>/...` and `/repository/<repository>/v2/<image>/...` both become `head.path = v2/<image>/...` on that repository.

## 7. Gaps

* The handler is registered by `wiring::formats::registry`, and `/v2`, `/v2/token` and `/v2/_catalog` are mounted by `ArtifactModule` (`docs/server.md`).
* Real `docker`, `skopeo` and `helm` runs are not part of this crate's tests. They belong behind `RUSTRYX_IT` later. Do not set that variable for these tests.
* Tag and referrer listings scan assets. A table keyed by repository, image and tag would make them index lookups. That would be a core change.
* A bearer token together with stored Basic credentials is not cached (section 2.5) until format-core can omit the stored `Authorization` on one call.
* `token/service/tests.rs` and several handler modules (`handler`, `uploads`, `manifests`, `mount`, `proxy::manifest`, `group`, `catalog`) have no tests of their own. The engine tests in section 5 cover the four negative paths and the allow list. They do not cover tag listing, referrers, group merge or a cached proxy pull.

## 8. Running the tests

```
cargo fmt -p rustryx-format-oci
cargo clippy -p rustryx-format-oci --all-targets --locked -- -D warnings
cargo test -p rustryx-format-oci --all-targets --locked
```

One cargo process. `jobs` stays at 5 (`.cargo/config.toml`). No `cargo clean`.
