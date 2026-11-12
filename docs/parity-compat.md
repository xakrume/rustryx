# Rustryx parity matrix: compat Repository and OCI registry

Status: research draft, 2026-10-24. Owner: product. Companion document: [ui-page-inventory.md](ui-page-inventory.md).

Purpose: the product owner requires that Rustryx covers at least the capabilities of compat Repository 3.x (Community feature set) and a typical OCI registry, with a quality web UI. This document lists what the two products offer according to their public documentation, assigns each capability a Rustryx target, and proposes where it lands in the codebase and in the native API.

## 1. Method and rules

- Only capabilities seen in the official documentation pages listed in the Sources section are included. A statement that could not be confirmed is written `unverified`.
- Vendor text is summarized in own words. Nothing is copied from the vendor documentation. Both products are only observed as public contracts; no vendor code, schema or UI is reused (README section 2, clean-room rule).
- compat documentation observed: the English compat help site, features up to release 3.97.0 are mentioned (service account tokens are marked new in 3.97.0). OCI-registry documentation observed: that product's public docs, whose selector marks 2.15.0 as the latest release.
- "ТЗ" is the Russian technical specification in `docs/TZ/nexspence-rust/` of the project docs (README.md first). "README" below always means that specification's README.
- The Rustryx scope baseline is README section 3 and section 8 of the specification, plus the iteration list in `07-implementation-prompt.md`. Where `01-architecture.md` lists something in its phase 1 table that README section 3 does not, the cell says `phase1` and the note names the source. README wins on a conflict (README section 8, norm at conflict).

### Legend for the compat Repository and OCI-peer columns

| Value | Meaning |
|---|---|
| yes (CE) | compat: the feature matrix lists it for Community Edition (formats, S3, search, access controls, LDAP, external PostgreSQL, backup, RUT, health check summary, content selectors, routing rules, REST API, UI upload) |
| yes (npl) | compat: described in the docs, not on the matrix's Pro-only lists and not on the Pro features page (npl = not Pro-listed); the edition is not explicitly confirmed |
| yes (Pro) | compat: Pro license only according to the matrix, the Pro features page or the page itself |
| yes (Pro+) | compat: Pro+ or Pro with the Repository Firewall add-on |
| yes (edition not stated) | compat: described, but absent from the matrix lists |
| yes | OCI peer: described in that product's docs; or compat when the feature is edition-neutral |
| no | the product has no such feature on the pages that were read |
| unverified | a page title exists or a snippet mentions it, but the content was not confirmed |

compat Community Edition is the free tier. The tiers page states an operational cap of 40,000 components for it. Statements about a requests-per-day cap conflict between pages and are treated as unverified. Rustryx has no such caps.

### Legend for the Rustryx target column

| Target | Meaning |
|---|---|
| `phase1` | already in the ТЗ phase 1 scope (README section 3.1, section 8.6, iterations 1 to 4 of the implementation prompt, or the 01-architecture phase 1 table) |
| `wave2` | to build next, after phase 1. Many of these are ТЗ phase 2 items (README section 3.2); the others are new requirements from the compat and OCI-registry parity requirement |
| `later` | worth building, not next; the reason is given |
| `out` | will not be built; the reason is given |

Each capability has a stable ID such as `REPO-03` so that the gap list in section 13 and the UI inventory can refer to it.

### Rustryx requirements that are not parity items

These come from the ТЗ, have no counterpart on the vendor pages read, and are therefore not rows in the matrix, but several rows depend on them: SSRF filtering of every outbound URL (upstream, replication endpoint, webhook target; README 8.6 and 03-security), OSV based scanning of Maven, npm, PyPI and Cargo packages (01-architecture phase 2; see section 11.7), group name claiming against dependency confusion (01-architecture 1.6.1), the Pub format with its 401 and 403 rules (README 8.5), and Keycloak and LDAP group to role mapping with the `origin` column (README 8.5).

### Summary of targets

| Area | `phase1` | `wave2` | `later` | `out` | Total |
|---|---|---|---|---|---|
| Repositories and formats | 20 | 19 | 8 | 4 | 51 |
| Blob stores and storage | 6 | 10 | 3 | 1 | 20 |
| Search and browse | 8 | 10 | 3 | 0 | 21 |
| Upload and component management | 5 | 10 | 1 | 0 | 16 |
| Security (authn, authz, tokens, TLS) | 31 | 22 | 4 | 10 | 67 |
| Cleanup, retention, quotas, GC, tasks | 10 | 13 | 2 | 0 | 25 |
| Proxy, routing, replication | 9 | 10 | 3 | 0 | 22 |
| Scanning, trust, SBOM | 1 | 23 | 4 | 1 | 29 |
| Operations, webhooks, audit, system, API, CLI | 12 | 18 | 3 | 7 | 40 |
| **All areas** | **102** | **135** | **31** | **23** | **291** |

### Native API naming

All proposed native resources live under `/api/v1/`, REST style, cursor pagination with `continuationToken` compatible naming, JSON bodies, an OpenAPI document served by the node. compat v1 compatibility endpoints stay under `/service/rest/v1/` and are separate (README section 3.1). Crate names are the existing workspace crates unless a new crate is proposed explicitly (proposed new crates: `rustryx-scan`, `rustryx-replication`, `rustryx-notify`, `rustryx-cli`).

## 2. Repositories and formats

The OCI peer has no repository types in the compat sense: its unit is a project that holds OCI repositories. The project model is discussed in section 11.1.

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `REPO-01` Hosted repository (authoritative store for own artifacts) | yes (CE) | yes: pushed content inside a project | `phase1` | `rustryx-core` Repository type hosted; `/api/v1/repositories` |
| `REPO-02` Proxy repository (cache on first request, serve from cache afterwards) | yes (CE) | yes, OCI only: proxy cache project | `phase1` | `rustryx-format-core` proxy pipeline; remote URL checked by the SSRF allowlist |
| `REPO-03` Group repository (one URL over ordered members, first match wins, groups can contain groups) | yes (CE) | no | `phase1` | `rustryx-format-core` group pipeline with index merge and name claiming (01-architecture 1.6.1) |
| `REPO-04` Group access rule: privilege on a group covers members only when requested through the group | yes (CE) | n/a | `phase1` | `rustryx-rbac` checks the repository named in the request; document the rule in the RBAC guide |
| `REPO-05` Repository online/offline switch | yes (npl) | no | `wave2` | `online` flag on `/api/v1/repositories/{name}`; offline returns 503 from the format router |
| `REPO-06` Per-repository blob store selection | yes (npl) | no: one storage backend per instance | `phase1` | `blob_store` field on the repository; `rustryx-blob` registry (README 3.1) |
| `REPO-07` Strict content type validation per format | yes (npl) | no | `wave2` | `strict_content_type` flag, enforced in each `FormatHandler::classify` |
| `REPO-08` Hosted write policy (allow redeploy, deploy once, deny) | yes (npl): deployment policy | partly: tag immutability rules (see `RET-10`) | `phase1` | `WritePolicy` allow/allow_once/deny in `rustryx-core` (01-architecture phase 1 table) |
| `REPO-09` Maven version policy (release, snapshot, mixed) and layout policy (strict, permissive) | yes (npl) | no | `phase1` | `format_config` of `rustryx-format-maven`; strict layout rejects non-conventional paths |
| `REPO-10` Maven: Content-Disposition inline or attachment | yes (npl) | no | `later` | per-repo header switch; low value, add when a contract test needs it |
| `REPO-11` Format: maven2 (hosted, proxy, group) | yes (CE) | no | `phase1` | `rustryx-format-maven` |
| `REPO-12` Format: Gradle | listed in the feature matrix only, no format page seen (`unverified`) | no | `phase1` | Gradle clients use the Maven repository layout; cover with a Gradle contract test on `rustryx-format-maven` |
| `REPO-13` Format: npm (hosted, proxy, group) | yes (CE) | no | `phase1` | `rustryx-format-npm` |
| `REPO-14` Format: PyPI (hosted, proxy, group) | yes (CE) | no | `phase1` | `rustryx-format-pypi` |
| `REPO-15` Format: Docker registry (hosted, proxy, group) | yes (CE) | yes: the core function | `phase1` | `rustryx-format-oci` serves docker and oci labels from one engine |
| `REPO-16` Format: OCI (hosted, proxy, group) | yes (CE) | yes | `phase1` | `rustryx-format-oci`; Referrers API per iteration 3 |
| `REPO-17` Format: Helm classic index and charts | yes (CE): hosted, proxy, group | charts as OCI artifacts yes (Helm 3.8 or later); classic chart repository support `unverified` (ChartMuseum status not found in the pages read) | `phase1` | `rustryx-format-helm`; OCI charts go through `rustryx-format-oci` |
| `REPO-18` Format: raw | yes (CE) | no | `phase1` | `rustryx-format-raw` |
| `REPO-19` Format: Pub (Dart and Flutter) | yes (CE): hosted, proxy, group, since 3.92.0 | no | `phase1` | `rustryx-format-pub`; Hosted Pub Repository Specification v2 |
| `REPO-20` Format: Cargo (Rust) | yes (CE): all three types | no | `wave2` | new `rustryx-format-cargo`; ТЗ phase 2 |
| `REPO-21` Format: Go modules | yes (CE): all three types | no | `wave2` | new `rustryx-format-go`; ТЗ phase 2 list in 01-architecture |
| `REPO-22` Format: APT (hosted and proxy, no group; hosted needs a GPG signing key and distribution name; flat remotes) | yes (CE): proxy and hosted | no | `wave2` | new `rustryx-format-apt`; signing key held as a secret reference, never in the DB in clear |
| `REPO-23` Format: Yum | yes (CE): all three types | no | `wave2` | new `rustryx-format-yum`; repodata generation in `spawn_blocking` (README 8.1) |
| `REPO-24` Format: NuGet | yes (CE): all three types | no | `wave2` | new `rustryx-format-nuget`; needs API key realm (`AUTH-44`) |
| `REPO-25` Format: Conan | yes (CE): all three types | no | `wave2` | new `rustryx-format-conan` |
| `REPO-26` Format: Conda | yes (CE): all three types | no | `wave2` | new `rustryx-format-conda` |
| `REPO-27` Format: Terraform (group support unclear: the table marks it, the description names only proxy and hosted; `unverified`) | yes (CE) | no | `wave2` | new `rustryx-format-terraform`; decide group support by contract test |
| `REPO-28` Format: RubyGems | yes (CE): all three types | no | `wave2` | new `rustryx-format-rubygems` |
| `REPO-29` Format: R (CRAN) | yes (CE): all three types | no | `wave2` | new `rustryx-format-cran` |
| `REPO-30` Format: Alpine | yes (CE): all three types | no | `wave2` | new `rustryx-format-alpine`; ТЗ phase 2 |
| `REPO-31` Format: Hugging Face (proxy only in compat) | yes (edition not stated; absent from the matrix format list): proxy only | no | `wave2` | new `rustryx-format-huggingface`; ТЗ phase 2; proxy first |
| `REPO-32` Format: Ansible Galaxy | yes (CE): all three types | no | `later` | not in the ТЗ; add through the one-crate format contract after the ТЗ phase 2 formats |
| `REPO-33` Format: CocoaPods (proxy only) | yes (CE): proxy only | no | `later` | not in the ТЗ; proxy-only format, add after the ТЗ phase 2 formats |
| `REPO-34` Format: Composer (PHP) | yes (edition not stated; absent from the matrix format list): all three types | no | `later` | not in the ТЗ; after the ТЗ phase 2 formats |
| `REPO-35` Format: Git LFS (hosted only) | yes (CE): hosted only | no | `later` | not in the ТЗ; after the ТЗ phase 2 formats |
| `REPO-36` Format: p2 (Eclipse, proxy only) | yes (CE): proxy only | no | `later` | not in the ТЗ; proxy-only format, add after the ТЗ phase 2 formats |
| `REPO-37` Format: Swift | yes (CE): all three types | no | `later` | not in the ТЗ; after the ТЗ phase 2 formats |
| `REPO-38` Format: Bower (legacy) | listed as legacy; support marks empty in the format table | no | `out` | legacy ecosystem; the compat table shows no proxy, hosted or group support for it |
| `REPO-39` Generic OCI artifacts (any config media type: Helm charts, CNAB, OPA bundles, ML models) with display metadata taken from manifest annotations and layer icons | yes (CE): OCI format | yes | `phase1` | the OCI engine stores any artifact type; annotation-driven UI metadata is `wave2` in `rustryx-format-oci` |
| `REPO-40` Multi-arch image index (children listed read-only) | yes | yes | `phase1` | OCI engine; children view is UI only (`wave2`) |
| `REPO-41` Docker path-based routing (`/repository/<name>` and `/v2/...`) | yes (npl): preferred method | yes: host plus project path | `phase1` | 01-architecture 1.6 URL spaces |
| `REPO-42` Docker subdomain routing | yes (Pro) | no | `wave2` | ТЗ phase 2 (01-architecture); Helm flag `docker.subdomainConnector.enabled` (OPS-35) |
| `REPO-43` Docker port connector per repository | yes (npl), legacy; page advises about 20 connectors at most | no | `out` | legacy; compat itself prefers path routing; one listener per repository does not fit stateless nodes |
| `REPO-44` Docker Registry API v1 fallback | yes (npl) | no | `out` | legacy protocol; compat needs it mainly for `docker search` and old clients |
| `REPO-45` Group deployment to a writable member (npm, Docker) | yes (Pro) | no | `later` | Pro only; clients can push to the hosted repository directly; revisit with staging |
| `REPO-46` Name claiming and dependency confusion defence in group merge | yes (Pro+): namespace confusion protection via Firewall | no | `phase1` | `GroupIndexNameClaimer` for pub (01-architecture 1.6.1); extend to npm, PyPI and Maven as `wave2` |
| `REPO-47` Repository action: invalidate proxy cache | yes (npl) | no | `wave2` | `POST /api/v1/repositories/{name}/actions/invalidate-cache` |
| `REPO-48` Repository action: rebuild search index | yes (npl) | no | `out` | search runs on PostgreSQL indexes updated in the same transaction (01-architecture 1.3); there is no separate index to rebuild |
| `REPO-49` Repository action: enable or disable health check per proxy | yes (npl) | no | `wave2` | tied to the scanning wave (`SCN-13`); `health_check` flag on the repository |
| `REPO-50` Default repositories created at first start (maven-central, maven-public and similar) | yes (npl) | no | `wave2` | optional first-run seed and a UI wizard; no default repositories without an explicit choice |
| `REPO-51` Repository description in Markdown and per-repository info tab | no | yes | `wave2` | `description` field on repositories and projects |

Sources: N-types, N-fields, N-formats, N-matrix, N-pub, N-docker, N-routing, N-actions, H-images, H-oci, H-projects.

## 3. Blob stores and storage

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `BLOB-01` File blob store on a local or network path (NFS v4, EFS) | yes (npl) | yes: filesystem backend (default) | `phase1` | `rustryx-blob` local store; HA needs a shared RWX volume (SRE-34) |
| `BLOB-02` S3 blob store: bucket, region, key prefix | yes (CE) | yes: S3 backend | `phase1` | `rustryx-blob` S3 store, multipart parts of 16 MiB (01-architecture 1.4) |
| `BLOB-03` S3-compatible endpoint URL and path-style access | yes (CE) | `unverified` (S3 backend is listed, endpoint option not seen) | `phase1` | SeaweedFS in the local stack, Ceph RGW and AWS S3 the same way; `endpoint` and `path_style` in `rustryx-config` |
| `BLOB-04` S3 credentials: static keys, session token, assume-role, instance metadata, environment | yes (CE) | unverified | `wave2` | phase 1 takes static keys and environment; assume-role and instance metadata added in `rustryx-blob` |
| `BLOB-05` S3 server-side encryption (SSE-S3, SSE-KMS with full key id) | yes (CE) | unverified | `wave2` | `encryption` block on the S3 blob store definition |
| `BLOB-06` S3 connection pool size and extra tuning | yes (CE) | thread limits in the storage section | `wave2` | pool settings in `BlobStoreDef` |
| `BLOB-07` Azure Blob blob store | yes (Pro) | yes (Azure backend) | `wave2` | new store in `rustryx-blob`; ТЗ phase 2 and 3 |
| `BLOB-08` Google Cloud Storage blob store | yes (Pro) | yes (GCS backend) | `later` | not in the ТЗ (only Azure is listed); add on demand behind the same `BlobStore` trait |
| `BLOB-09` Swift or Alibaba OSS storage | no | yes (Swift, OSS backends) | `out` | peer-only backends with no compat parity need; revisit on demand |
| `BLOB-10` Blob store group with fill policy (round robin, write to first) | yes (Pro) | no | `wave2` | `BlobStoreGroup` in `rustryx-blob`; ТЗ phase 2. Design in section 12.9 |
| `BLOB-11` Convert a blob store into a group (irreversible) and remove a member with data migration | yes (Pro) | no | `wave2` | admin operation plus a task `remove-blob-store-member`; irreversible conversion needs a confirmation step in the UI |
| `BLOB-12` Soft quota per blob store (alert only; limit by used space or by remaining space) | yes (npl) | no (quotas are per project, see `RET-11`) | `wave2` | `quota` on the blob store; surfaces in `/readyz` details, metric and `/api/v1/system/status`; never blocks writes |
| `BLOB-13` Blob store metrics: name, type, health state, blob count, used and available size | yes (npl) | quota usage metrics | `phase1` | `blob_store_used_bytes`, `blob_store_free_bytes` (SRE-08); list view in `/api/v1/blob-stores` |
| `BLOB-14` Soft delete then compact (permanently remove deleted blobs, optional minimum age in days) | yes (npl): compact task with an age setting | yes: garbage collection | `phase1` | blob GC with minimum age (ADR-3, iteration 4); `gc.min_age` default 24 h |
| `BLOB-15` Cleanup of orphaned asset blobs and temporary files | yes (npl): tasks | `unverified` (upload purging is named in the config page, details not seen) | `phase1` | the same GC job; abandoned uploads removed after `gc.min_age` (01-architecture 1.4) |
| `BLOB-16` Pre-signed URL redirect for downloads (expiry 5 minutes by default) | yes (npl) | redirect setting in the storage section | `wave2` | off by default; never for pub (Authorization prefix rule), never while a proxy cache fill is running |
| `BLOB-17` S3 failover buckets in other regions | yes (Pro) | no | `later` | Pro only; bucket replication is configured outside the product in the compat docs |
| `BLOB-18` Change the blob store of a repository while online (task; proxy and group supported on newer versions) | yes (Pro) | no | `later` | ТЗ phase 3; the opaque blob key plus `blobs` table (ADR-3) makes a live move possible |
| `BLOB-19` Reconcile database against blob store, recalculate sizes | yes (npl): repair tasks, advised only with vendor support | no | `wave2` | admin task `verify-blobs` that reports missing and orphaned blobs; repair only after confirmation |
| `BLOB-20` Download counter and last-downloaded timestamp per asset (30-day count shown) | yes (npl) | pull time tracked for retention | `wave2` | counter flushed every 10 s (01-architecture 1.4, phase 2 table); needed by the cleanup criterion `last_downloaded` |

Sources: N-blob, N-s3, N-assetinfo, N-cleanup, N-changebs, N-tasks, N-pro, H-install, H-gc, H-settings.

## 4. Search and browse

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `SRCH-01` Keyword quick search over component metadata (trailing wildcard with a minimum prefix, quoted phrase) | yes (CE) | list filters only (type, tag, label); a global keyword search `unverified` | `phase1` | PostgreSQL full text plus trigram indexes (01-architecture 1.3); `GET /api/v1/search/components?q=` |
| `SRCH-02` Advanced search by format, repository, group, name, version | yes (CE) | filters on the artifact list (type, tags, labels) | `phase1` | same endpoint with filter parameters; compat v1 `search` compat under `/service/rest/v1/search` (README 3.1) |
| `SRCH-03` Search by checksum (MD5, SHA-1, SHA-256, SHA-512) | yes (CE) | by digest in the Security Hub search | `phase1` | SHA-256, SHA-1, MD5 are stored in the `blobs` table (01-architecture 1.3); SHA-512 is not stored, so it is `later` unless a format needs it |
| `SRCH-04` Format-specific search fields (Maven coordinates, Docker tag, digest, layer, OS, architecture, labels, NuGet id and others) | yes (CE) | tag and label filters only | `wave2` | JSONB `attributes` with expression indexes per format; phase 1 ships Maven and Docker names only |
| `SRCH-05` Search implemented as database queries, no external search engine; relevance ranking needs PostgreSQL | yes (npl): since 3.88.0 | no search engine seen | `phase1` | native fit: Rustryx is PostgreSQL only; ranking via `ts_rank` |
| `SRCH-06` Result cap and paging (first 300 results shown in the UI) | yes (CE) | `unverified` | `phase1` | cursor paging with `continuationToken` (01-architecture 1.6) |
| `SRCH-07` Search by component tag | yes (Pro) | filter artifacts by tag and by label | `wave2` | depends on labels and tags (`UPL-09`, `UPL-11`) |
| `SRCH-08` Search REST API for components and assets | yes (CE) | API explorer (REST API 2.0); search endpoints not read | `phase1` | `/api/v1/search/components`, `/api/v1/search/assets` and compat compat |
| `SRCH-09` Browse repository list with type, format, status, URL, health, firewall status; sort and filter | yes (npl) | project repositories tab | `phase1` | `GET /api/v1/repositories` with status fields; health columns appear in `wave2` |
| `SRCH-10` Browse tree of repository contents (folders, up to 10,000 items per level, searchable) | yes (npl) | repository and artifact lists, no folder tree | `wave2` | `GET /api/v1/repositories/{name}/tree?path=&continuationToken=`; prefix plus delimiter query over assets, keyset paging; design in section 12.7 |
| `SRCH-11` Browse permission semantics (browse for UI and search, read for clients) | yes (npl) | role based | `phase1` | actions browse, read, write, delete in `rustryx-rbac` (01-architecture 1.5) |
| `SRCH-12` Component detail: group, name, version, assets list, delete component | yes (npl) | artifact detail page (tags, vulnerabilities, build history) | `wave2` | `GET /api/v1/components/{id}` with embedded assets; delete via `DELETE` |
| `SRCH-13` Component popularity and age (needs health check data) | yes (CE): only with health check enabled | no | `later` | needs external popularity data that Rustryx does not have; age alone is cheap and ships with `SRCH-12` |
| `SRCH-14` Asset detail: path, content type, size, blob created and updated, last downloaded, 30-day download count, blob reference, uploader and uploader IP, checksums, format attributes | yes (npl) | artifact digest, annotations and push time shown in lists | `wave2` | `GET /api/v1/assets/{id}`; store uploader user id by default, client IP only if audit config allows (privacy) |
| `SRCH-15` Image build history and config view for OCI images | no | yes: build history is a project permission row; detail page `unverified` | `wave2` | read from the image config blob; `GET /api/v1/components/{id}/config` |
| `SRCH-16` Artifact list filter: tagged or untagged, by type, by label | no | yes | `wave2` | query parameters on `/api/v1/components` for OCI repositories |
| `SRCH-17` Index images shown with referenced children in a read-only view | no | yes | `wave2` | UI over OCI manifests; `GET /api/v1/components/{id}/children` |
| `SRCH-18` Copy repository URL or pull command from the list | yes (npl): copy URL | yes: copy pull command | `wave2` | `client setup` panel fed by `GET /api/v1/client-setup?repository=` (see UI inventory) |
| `SRCH-19` Simple HTML directory view of a repository at its own endpoint | yes (npl) | no | `later` | the web UI tree supersedes it; Raw and Maven directory listings are served as needed by clients |
| `SRCH-20` Deleted items restorable from a trash view | yes (npl): browse page mentions restore from Trash | no | `later` | soft delete with a grace period exists through GC `min_age`; a restore workflow and UI need design; compat also lists accidental-deletion rollback as a Pro feature in the tier page |
| `SRCH-21` Repository description (Markdown) shown on a repository info tab | no | yes | `wave2` | same field as `REPO-51` |

Sources: N-search, N-sql, N-browse, N-compinfo, N-assetinfo, N-tagging, N-tiers, H-images, H-roles, H-hub, H-api.

## 5. Upload and component management

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `UPL-01` Upload components through the web UI (hosted repositories only, format-specific forms, allowed file types per format) | yes (CE) | no: images enter by `docker push`; no UI upload | `wave2` | `POST /api/v1/components?repository=` multipart with streaming body; UI Upload page |
| `UPL-02` Multi-file upload for Maven and Raw, Maven coordinates form, POM read for coordinates | yes (CE) | no | `wave2` | coordinates from `pom.xml` in the uploaded set; no POM generation (same as compat) |
| `UPL-03` Formats that cannot be uploaded through the UI (Docker, OCI, Cargo, Conan, Git LFS, Hugging Face, p2, Bower, CocoaPods) | yes (CE) | n/a | `wave2` | the UI hides the upload form for these formats; clients use native protocols |
| `UPL-04` Component upload through the REST API | yes (CE) | via OCI push | `phase1` | compat v1 `components` upload per README 3.1; native `/api/v1/components` in `wave2` |
| `UPL-05` Publish through native client protocols (mvn deploy, npm publish, twine, docker push, helm push, dart pub publish) | yes (CE) | docker push and helm push | `phase1` | REQ-08 e2e tests with real clients |
| `UPL-06` Upload permission model (upload privilege plus edit on the repository) | yes (npl) | developer role and above | `phase1` | `write` on the repository selector; no extra upload privilege |
| `UPL-07` Attach existing tags at upload time | yes (Pro) | no | `wave2` | with tags (`UPL-09`) |
| `UPL-08` Delete component or asset | yes (npl) | delete artifact, delete tag only (detag) | `phase1` | compat v1 `components` and `assets` delete; OCI tag delete in the OCI engine |
| `UPL-09` Component tags (named labels grouping components, for example a build id; REST CRUD; attributes; associate by search) | yes (Pro) | no: labels (below) serve a similar need | `wave2` | `/api/v1/tags` and `/api/v1/tags/{tag}/components`; see the label and tag decision in section 11.9 |
| `UPL-10` Artifact labels, global and per project, applied to artifacts, filterable | no | yes | `wave2` | `/api/v1/labels` (scope system or project); same table as tags with a `scope` column |
| `UPL-11` Cleanup of tags by age or name pattern, with optional component deletion | yes (Pro): task | no | `wave2` | task type `cleanup-tags`; delete components only with an explicit flag |
| `UPL-12` Staging and promotion (move components by search criteria or by tag between repositories; metadata-only move when blob stores match; 10,000 per move cap) | yes (Pro) | partial: copy and retag; replication | `wave2` | `POST /api/v1/promotions` (move and copy); ТЗ phase 2 staging and promotion |
| `UPL-13` Copy or retag an artifact into another project or repository (read on source, write on target) | no | yes | `wave2` | `POST /api/v1/promotions` with `mode=copy`; OCI uses manifest mount, no blob copy |
| `UPL-14` Add and remove tags (OCI) on an artifact, detag without deleting | no | yes | `phase1` | OCI engine tag operations; UI actions in `wave2` |
| `UPL-15` Preheat images into a P2P engine (Dragonfly, Kraken) by policy, with executions and logs | no | yes | `later` | needs external P2P engines and is not part of compat parity; revisit on demand |
| `UPL-16` Client instructions per format (pull and push commands) | no in-product instructions seen; only a copy-URL action | pull command copy in the artifact list; a push command panel `unverified` | `wave2` | one reusable `client setup` panel for mvn, npm, pip, docker, helm, pub, raw |

Sources: N-upload, N-tagging, N-staging, N-auto, N-matrix, H-images, H-p2p, H-ocihelm.

## 6. Security: authentication, authorization, tokens, TLS

### 6.1 Authentication realms, users and identity providers

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `AUTH-01` Local users (create, active or disabled status, roles, set password) | yes (npl) | yes: database auth, admin creates users | `phase1` | `rustryx-auth` local provider, argon2id; `/api/v1/users` and compat v1 `security/users` |
| `AUTH-02` Several identity sources active at once, tried in a configurable order | yes (npl): ordered realm list | no: one mode only; database mode is locked once local users exist | `phase1` | the credential chain in `rustryx-auth`; ordering is fixed by code in phase 1, editable list `/api/v1/auth/realms` in `wave2` |
| `AUTH-03` LDAP and Active Directory bind authentication | yes (CE) | yes | `phase1` | `rustryx-auth::ldap`; LDAPS or StartTLS only (README 8.4) |
| `AUTH-04` LDAP: several servers with an order | yes (npl) | no (one server) | `wave2` | `/api/v1/auth/providers/ldap` becomes a list with `priority` |
| `AUTH-05` LDAP connection templates (Active Directory, generic, POSIX) | yes (npl) | no | `wave2` | UI presets that fill filters and attributes; no server-side logic |
| `AUTH-06` LDAP "verify user mapping" and "verify login" test tools | yes (npl) | no | `wave2` | `POST /api/v1/auth/providers/ldap/{id}/verify` returns up to 20 users or a login trace without secrets |
| `AUTH-07` LDAP groups: static (group lists members) and dynamic (membership attribute on the user) | yes (npl) | yes: group filter, group id attribute, membership attribute (memberOf default), search scope | `phase1` | `GroupResolver` with `memberOf` and reverse `member` search (iteration 1) |
| `AUTH-08` LDAP admin group giving the top role | yes (npl): by external role mapping | yes: group admin DN | `phase1` | `auth.ldap.admin_groups` exact DN match gives `nx-admin` (README 8.5) |
| `AUTH-09` LDAP group search in parallel to avoid login timeouts | no | yes | `later` | only matters with very many groups; the 5 s timeouts and the no-empty-on-error rule (README 8.4) come first |
| `AUTH-10` Cache of successful LDAP authentications with a clear-cache action | yes (npl) | no | `out` | README 8.4 and 8.5 bind per login and cap the group set at 5 minutes; caching password results is a security regression |
| `AUTH-11` LDAP SASL mechanisms (DIGEST-MD5, CRAM-MD5) and anonymous bind | yes (npl) | no | `out` | obsolete or weak; README 8.4 requires TLS and a non-empty password |
| `AUTH-12` LDAPS with a custom CA and server certificate verification | yes (npl): truststore | yes: verify-cert switch | `phase1` | CA bundle in config; no skip-verify switch in production builds (SEC-26) |
| `AUTH-13` OpenID Connect login (authorization code) with group-to-role mapping | yes (Pro): since 3.86.0 | yes | `phase1` | `rustryx-auth::oidc`, Keycloak first; code flow with PKCE (README 8.3) |
| `AUTH-14` OIDC bearer access tokens accepted by the API and CLI (JWT checked against the provider keys) | yes (Pro): OAuth2 realm uses JWT tokens | no: CLI uses a per-user secret instead | `phase1` | JWKS validation, RS256 and ES256 only (README 8.3) |
| `AUTH-15` OIDC user auto-creation at first login | yes (Pro) | yes: auto onboard with a username claim | `phase1` | JIT modes jit, allowlist, manual (01-architecture 1.5); lookup by `(iss, sub)` only |
| `AUTH-16` OIDC group claim to roles; admin group; exact group names | yes (Pro) | yes: group claim name, admin group | `phase1` | `idp_group_mappings`; `auth.oidc.admin_groups` full path match (README 8.5) |
| `AUTH-17` OIDC group filter by regular expression | no | yes | `out` | unmapped groups already grant nothing (README 8.5); a regex filter would only add a way to over-match |
| `AUTH-18` OIDC provider session logout from the product logout (logout URL) | yes (Pro) | yes: user logout option | `wave2` | RP-initiated logout; back-channel logout stays phase 3 (01-architecture) |
| `AUTH-19` OIDC configuration screen (endpoints, client id and secret, scopes, claim names, certificate verification, redirect URI shown) | yes (Pro): UI fields | yes: provider name, endpoint, client id and secret, scopes | `phase1` | config file in phase 1 (README 5); `/api/v1/auth/providers/oidc` editable in `wave2` with secret references only |
| `AUTH-20` SAML 2.0 single sign-on (UI only, tokens needed for tools) | yes (Pro) | no | `later` | README 3.3 excludes it and 01-architecture lists it in phase 3; Keycloak can broker SAML identity providers into OIDC |
| `AUTH-21` Atlassian Crowd realm | yes (Pro) | no | `out` | vendor-specific directory; LDAP and OIDC cover the need |
| `AUTH-22` Remote user token realm (identity taken from a trusted reverse proxy header) | yes (CE) | no | `wave2` | accept the header only from a configured trusted-proxy CIDR list, otherwise ignore it; authorization still from groups |
| `AUTH-23` UAA authentication | no | listed in the UI, not recommended, undocumented | `out` | undocumented and not recommended by that product itself |
| `AUTH-24` Default role granted to every authenticated user | yes (npl): capability | no | `wave2` | explicit setting, must never grant `nx-admin`, audited; OIDC `default_role` for unmatched users already exists in README 5 |
| `AUTH-25` Self-registration of users from the login page | no | yes: optional, off by default | `out` | identity comes from the identity provider or an admin; no public sign-up surface |
| `AUTH-26` Admin account recovery (reset the admin password procedure) | yes (npl): page exists, content not read | no | `phase1` | break-glass local admin from a bootstrap secret (README 8.3); runbook step |
| `AUTH-27` Authentication attempt limiting (429 with Retry-After, growing delay, counter reset on success; per node in compat) | yes (npl): defaults 3 attempts, 30 s base, 900 s cap | no | `phase1` | login guard in PostgreSQL shared across nodes (README 8.5); stronger than the per-node compat counter |
| `AUTH-28` IP allow list (monitor or enforce, CIDR entries, CSV import, 256 entries) | yes (Pro) | no | `later` | Pro only; ingress or network policy covers it; revisit with a trusted-proxy design |
| `AUTH-29` Session settings (UI timeouts, session length, title) | yes (npl): UI settings capability | no | `phase1` | fixed by README 8.3: 30 minutes idle, 8 hours absolute |
| `AUTH-30` User list with source filter; edit roles of external users locally | yes (npl) | users tab, delete removes peer-registry data only | `phase1` | `/api/v1/users?source=`; external identity rows are read-only except roles with origin `manual` |
| `AUTH-31` Self-service profile: change name, email and password; tokens | yes (npl): password only for the local realm | profile page with CLI secret | `wave2` | `/api/v1/me` and `/api/v1/me/tokens` |

### 6.2 Roles, privileges and content selectors

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `AUTH-40` Roles with nesting (a role includes other roles); built-in admin and anonymous roles not editable | yes (CE): custom access controls | no: five fixed project roles | `phase1` | `role_roles` table, `nx-admin`, `nx-anonymous`, `nx-developer` (01-architecture 1.5) |
| `AUTH-41` External role mapping (identity provider group to role) | yes (npl) | partly: admin group for LDAP and OIDC; group to project role mapping not confirmed (`unverified`) | `phase1` | `idp_group_mappings` table, edited through the API and audited (README 8.5) |
| `AUTH-42` Fixed project roles: limited guest, guest, developer, maintainer, project admin; system admin above them | no | yes | `wave2` | project roles are templates that expand to privileges plus selectors; design in section 11.1 |
| `AUTH-43` Privilege type: repository view (browse, read, add, edit, delete) per repository or format | yes (CE) | role matrix | `phase1` | actions browse, read, write, delete; compat add and edit both map to write in the compat layer |
| `AUTH-44` Privilege type: repository admin (configure repositories) | yes (CE) | project admin | `phase1` | admin routes require `nx-admin` in phase 1 (01-architecture 1.5); per-repository admin privilege is `wave2` |
| `AUTH-45` Privilege type: repository content selector | yes (CE) | n/a | `phase1` | `rustryx-rbac` selector privilege; CEL-like language (07 iteration 1) |
| `AUTH-46` Privilege type: application (UI and settings features, create/read/update/delete) | yes (CE) | n/a | `wave2` | application privileges per admin area (users, roles, tasks, settings, audit); phase 1 has the single `nx-admin` gate |
| `AUTH-47` Privilege type: script (Groovy script REST API) | yes (npl): disabled by default, execute task disabled since 3.21.2 | no | `out` | arbitrary code execution surface; automation goes through typed APIs |
| `AUTH-48` Wildcard privilege (all functionality) | yes (CE) | system admin flag | `phase1` | `nx-admin` role (README 8.5); no free-form wildcard strings |
| `AUTH-49` Uploader metadata read privilege (show who uploaded) | yes (npl): since 3.93.0 | no | `later` | pairs with asset uploader display (`SRCH-14`); add when audit privacy settings exist |
| `AUTH-50` Privilege list, create, edit, delete in the UI | yes (CE) | n/a | `phase1` | `/api/v1/privileges`; compat v1 `security/privileges` |
| `AUTH-51` Content selector expression language: format, path, equality, prefix, regular expression, and, or, grouping | yes (CE): CSEL | repository and tag name patterns in rules | `phase1` | CEL-like with `format`, `path`, `coordinate.*`; evaluation limits SEC-18 |
| `AUTH-52` Format-specific selector variables (Maven coordinates, NuGet id, Swift scope, Terraform namespace) | yes (CE) | n/a | `phase1` | `coordinate.*` variables per format crate (07 iteration 1) |
| `AUTH-53` Selector preview against a repository (capped at 10 results) | yes (CE) | n/a | `wave2` | `POST /api/v1/content-selectors/preview` with a result cap |
| `AUTH-54` Selectors must allow parent folders for tree browsing; selectors never revoke other grants | yes (CE) | n/a | `wave2` | the browse tree API applies prefix-containment semantics; additive-only model already fixed (01-architecture 1.5) |
| `AUTH-55` Project membership with a role per member; project public or private | no | yes | `wave2` | `/api/v1/projects/{id}/members`; public maps to anonymous read (section 11.1) |
| `AUTH-56` Project-scoped permission matrix (view settings, manage members, push, pull, retag, scan, delete, webhooks, retention, immutability, quotas) | no | yes | `wave2` | the matrix is the starting set for project role templates; some rows were uncertain in the page text |

### 6.3 Tokens, robot accounts and keys

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `AUTH-60` Per-user token as a credential pair instead of a password (expiry 1 to 999 days, reset, admin can reset all) | yes (Pro): user tokens | yes: per-user CLI secret for OIDC users | `phase1` | `ryx_` API tokens with SHA-256 storage, expiry, revoke, last used (07 iteration 1) |
| `AUTH-61` Require tokens for all repository access (no password use by tools) | yes (Pro) | no | `wave2` | setting that rejects Basic with passwords on format routes; interacts with the Docker exception |
| `AUTH-62` Service account tokens bound to one role, expiry 30, 60, 90 days, one year or never, shown once, revoke, audited, daily purge of expired | yes (Pro): since 3.97.0 | no | `wave2` | token kind `service` in `/api/v1/api-tokens`; see the robot decision in section 11.2 |
| `AUTH-63` System robot accounts (all projects including future ones, or selected projects with per-project permissions) | no | yes | `wave2` | token kind `robot` with `scope` list of project and permission pairs |
| `AUTH-64` Project robot accounts (single project; name prefix, project name, account name) | no | yes | `wave2` | `kind=robot`, `project=<id>`; name pattern configurable |
| `AUTH-65` Robot permissions combine system-level and project-level actions; push requires pull; cannot exceed the creator's permissions; cannot sign in to the UI | no | yes | `wave2` | validate at creation against the creator's effective privileges; robot principals are rejected on the session login route |
| `AUTH-66` Robot secret shown once, refresh secret (random or manual), disable without delete | no | yes | `wave2` | `POST /api/v1/api-tokens/{id}/actions/rotate`; only SHA-256 is stored |
| `AUTH-67` Default robot expiry (30 days) and name prefix are system settings; never-expire option | no | yes | `wave2` | `settings.security.token_default_ttl`, `token_name_prefix` |
| `AUTH-68` Legacy robot accounts from before that product's 2.2 | no | yes (limited, cannot be edited) | `out` | legacy format of another product |
| `AUTH-69` Token revocation and "valid after" cut-off for sessions | yes (Pro): revoke | yes: disable and delete | `phase1` | `tokens_valid_after` (01-architecture 1.3) and `tid` revocation |
| `AUTH-70` Per-format credentials: npm bearer, NuGet API key, Conan, Ansible, OCI, pub, Terraform and Hugging Face tokens as realms | yes (npl) | docker and helm login with user or robot credentials | `phase1` | `ryx_` as password for Basic or Bearer works for maven, npm, pypi, pub, raw (README 3.1); NuGet API key `wave2` with the nuget crate |
| `AUTH-71` Token endpoint for `docker login` and `helm registry login` | yes (npl): Docker and OCI bearer realms | yes | `phase1` | `/v2/token` (01-architecture 1.6) |

### 6.4 Anonymous access and TLS

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `AUTH-80` Anonymous access switch, anonymous user, read-only default role | yes (npl) | yes: public projects are readable by anyone | `phase1` | both `auth.anonymous_enabled` and repository `allow_anonymous` must agree; anonymous write is never allowed (01-architecture 1.5) |
| `AUTH-81` Docker anonymous pull needs a per-repository setting in addition | yes (npl) | public project flag | `phase1` | same two-switch model, plus the `/v2/` challenge behaviour |
| `AUTH-82` Public or private project flag switchable at any time | no | yes | `wave2` | project `visibility` writes the repositories' `allow_anonymous` |
| `AUTH-83` Inbound HTTPS in the product (keystore, TLS 1.2 only by default, SNI) | yes (npl) | yes: HTTPS certificate and key in the install config | `phase1` | TLS 1.2 or higher through rustls, optional native listener; usually terminated at ingress (SEC-26) |
| `AUTH-84` TLS between internal components | no | yes: internal TLS option | `out` | single binary; TLS to PostgreSQL, Redis, S3, Keycloak and LDAP is mandatory with verification (SEC-26) |
| `AUTH-85` Managed outbound trust store (load certificate from a server or a PEM, per-proxy "use the truststore" switch) | yes (npl) | verify-remote-certificate switch per endpoint | `wave2` | `/api/v1/trusted-certificates`; PEM upload by default, any "fetch from server" helper must pass the SSRF filter |
| `AUTH-86` Skip certificate verification for upstream | no (use truststore instead) | yes: switch for self-signed endpoints | `out` | forbidden in production by SEC-26; use a CA bundle |

Sources: N-realms, N-users, N-roles, N-privileges, N-selectors, N-anon, N-ldap, N-oidc, N-saml, N-rut, N-utokens, N-satokens, N-ratelimit, N-iprules, N-defrole, N-caps, N-profile, N-matrix, N-pro, N-tiers, N-admin, N-ssl-search, H-auth, H-users, H-roles, H-robot, H-projcfg, H-settings, H-install.

## 7. Cleanup, retention, immutability, quotas, garbage collection, scheduled tasks

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `RET-01` Cleanup policy with criteria: component age, days since last download, release type, retained versions, asset name matcher; all criteria must match; criteria differ per format | yes (npl); retained versions is Pro | tag retention rules (see `RET-05`) | `phase1` | README 3.1; `cleanup_policies` table; phase 1 criteria: age, version count, name matcher; `last_downloaded` needs `BLOB-20` |
| `RET-02` Several policies attached to one repository; policies created by admins, attached by repository editors | yes (npl) | project-level rules | `phase1` | `cleanup_policy_ids` on the repository (01-architecture 1.3) |
| `RET-03` Preview of what a policy would delete (point-in-time, CSV, one-minute timeout) | yes (npl) | yes: dry run for retention | `phase1` | dry-run report first, then execution (DSO-34, iteration 4); report is stored, not only streamed |
| `RET-04` Cleanup execution as a scheduled task, soft delete, plus a task for orphaned blobs; default schedule differs between two compat pages (hourly vs daily at 1 am, `unverified`) | yes (npl) | schedule per project | `phase1` | `rustryx-tasks` cleanup job under distributed lock (REQ-07); default schedule documented once |
| `RET-05` Tag retention rules: keep N most recently pushed, keep N most recently pulled, keep pushed within N days, keep pulled within N days; matching or excluding selectors on repository and on tag; untagged option | no | yes | `wave2` | `/api/v1/retention-policies`; evaluated on tags, applied to whole artifacts; design in section 11.3 |
| `RET-06` Retention semantics: several rules combine with OR, results merged; one schedule per project; up to 15 rules; dry run and run now; rules not undoable | no | yes | `wave2` | same engine as `RET-01` with a `retain` mode; dry-run reports are mandatory before a first real run |
| `RET-07` Retain select versions in cleanup policies | yes (Pro) | no | `wave2` | `keep_versions` criterion shared with `RET-05` |
| `RET-08` Maven snapshot cleanup: minimum count to keep, retention days, removal after release, grace days | yes (npl): delete SNAPSHOT task | no | `wave2` | Maven policy template on top of `RET-01`; replaces the deprecated "unused snapshots" task |
| `RET-09` Cleanup by usage: days since last download, publish date used when never downloaded | yes (npl) | pulled-within and most-recently-pulled templates | `wave2` | requires the download counter (`BLOB-20`); scanners must not refresh the pull time (`SCN-21`) |
| `RET-10` Tag immutability rules: matching or excluding selectors; immutable tags cannot be overwritten, deleted, re-tagged or replaced by replication; up to 15 rules; OR logic | no | yes | `wave2` | `/api/v1/immutability-rules`; complements `REPO-08` write policy; see section 11.4 |
| `RET-11` Project storage quota: global default, per-project override, minus one means unlimited, usage counted per project (shared blobs counted once per project), manifest pushed last | no | yes | `wave2` | `/api/v1/quotas`; checked before write by the declared size and after write by actual size (01-architecture 1.4); see section 11.5 |
| `RET-12` Quota enforcement races: first manifests win under concurrent pushes | no | yes (documented limitation) | `wave2` | enforce in the same transaction that publishes the manifest, so a failed quota check leaves no visible artifact |
| `RET-13` Quota visible to users (usage and limit) | no | yes: quotas visible to all five roles | `wave2` | `GET /api/v1/projects/{id}/quota` |
| `RET-14` Garbage collection of unreferenced blobs: manual run, schedule (hourly, daily, weekly, cron), dry run with estimated reclaimed space, delete untagged artifacts first, worker count, safety window for fresh uploads, history with logs, stop | partial: compact blob store task (`RET-15`), no dry run seen | yes | `phase1` | blob GC with minimum age and dry-run (iteration 4); run history in `wave2`; design in section 11.12 |
| `RET-15` Compact blob store task with a "blobs older than N days" setting | yes (npl) | n/a | `phase1` | folded into `RET-14`: GC deletes only blobs unreferenced and older than `gc.min_age` (SRE-21) |
| `RET-16` Docker maintenance tasks: delete incomplete uploads, delete unused manifests and layers | yes (npl) | GC untagged option | `phase1` | part of GC; abandoned uploads removed after `gc.min_age` (01-architecture 1.4) |
| `RET-17` Scheduled task framework: frequencies manual, once, hourly, daily, weekly, monthly, cron; enable or disable; status and last result; failure or success email; task logs kept 30 days | yes (npl) | job service with schedules for GC, scans, retention, replication | `phase1` | `rustryx-tasks` scheduler with distributed locks (README 8.2); admin API and notifications in `wave2` |
| `RET-18` Task catalog: database backup, cleanup, compact blob store, change blob store, delete temp files, docker and Maven tasks, PyPI checksum and index tasks, Apt and Helm metadata rebuild, replicate, import and export assets, repair tasks | yes (npl): Pro for import, export, replicate | n/a | `wave2` | typed task registry with a `/api/v1/tasks` list; only the tasks that make sense for the Rustryx data model (no search or browse rebuild) |
| `RET-19` Task run, stop, and per-run log | yes (npl): run and stop controls not covered on the pages read (`unverified`); a search rebuild can be cancelled | stop for GC, scans, purge, replication | `wave2` | `/api/v1/tasks/{id}/runs`, `POST .../actions/run` and `.../stop`; cooperative cancel token (01-architecture 1.4) |
| `RET-20` Job queue dashboard: queues with pending count and latency, schedules, workers, pause and resume schedules, stop and free workers | no | yes | `later` | PostgreSQL lease queues (`SKIP LOCKED`) can expose a read-only view in `wave2`; pause and resume controls later |
| `RET-21` Audit log purge with retention window, schedule and dry run | rotates daily, keeps 90 days (file) | yes | `phase1` | partitions dropped by retention (01-architecture 1.3); a purge dry run is `out` because partition drop is all or nothing |
| `RET-22` Repository export (assets to a directory, incremental, optional unused-asset archive) and import of external files | yes (Pro) | no | `wave2` | ТЗ phase 2 export and import; tasks `export-repository` and `import-repository`; one import or export at a time |
| `RET-23` Backup and restore: database plus blob stores kept consistent; restore then reconcile | yes (CE): improved backup and restore; PostgreSQL tools external | yes: Velero based, read-only mode during backup, crash-consistent only | `wave2` | ТЗ phase 2; runbook per SRE-38 plus `rustryx backup verify`; needs read-only mode (`OPS-22`) |
| `RET-24` Recovery mode (blocks conflicting tasks while data is reconciled) | yes (npl): since 3.90.0, self-hosted | no | `later` | depends on the reconcile tooling (`BLOB-19`) |
| `RET-25` Upgrade path: scripted migrate of configuration and schema | yes (npl) | yes: prepare image `migrate` command | `phase1` | `rustryx migrate`, expand and contract (README 8.2) |

Sources: N-cleanup, N-tasks, N-mvnsnap, N-tagging, N-export, N-recovery, N-matrix, N-pro, H-retention, H-immutability, H-quota, H-gc, H-jobs, H-logrot, H-backup, H-upgrade, H-audit.

## 8. Proxy, routing rules, replication and registry endpoints

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `PRX-01` Proxy cache with maximum component age and maximum metadata age (minutes) | yes (npl) | revalidation by a lightweight HEAD on each pull | `phase1` | per-repository `proxy_config`; metadata TTL differs from blob TTL |
| `PRX-02` Negative cache of not-found responses with TTL | yes (npl) | no | `phase1` | iteration 2 of the implementation prompt |
| `PRX-03` Auto-block of an unreachable upstream with periodic retest, plus a manual block | yes (npl) | no | `phase1` | circuit breaker with half-open probes (iteration 2); manual `blocked` flag is `wave2` |
| `PRX-04` Serve cached content when the upstream is down | no (only the auto-block is described) | yes | `phase1` | serve-stale with a marker header (iteration 2) |
| `PRX-05` Upstream credentials (basic, preemptive) | yes (npl) | registry endpoint access id and secret | `phase1` | secrets as references, never returned by the API (SEC-25) |
| `PRX-06` Upstream HTTP settings: user-agent suffix, timeout, retries, redirects, cookies | yes (npl) | no | `phase1` | timeouts and retry rule per README 8.2; user-agent suffix `wave2` |
| `PRX-07` Outbound HTTP and HTTPS proxy with auth and an exclusion list (wildcards); NTLM option | yes (npl) | proxy and no-proxy settings for Trivy, replication and core | `wave2` | `settings.http_proxy`; NTLM is `out` (legacy Windows auth) |
| `PRX-08` Preserve percent-encoded characters when forwarding | yes (npl) | no | `later` | format specific; handle when a contract test fails |
| `PRX-10` Routing rules: allow or block mode, regular expression on the request path, one rule per proxy, shared between proxies, applies inside groups | yes (CE) | no | `phase1` | 01-architecture phase 1 table; `routing_rule_id` on the repository; use a linear-time regex engine because compat warns about ReDoS |
| `PRX-11` Routing rule test tool (enter paths, see allow or block) | yes (CE) | no | `wave2` | `POST /api/v1/routing-rules/{id}/test` |
| `PRX-12` Proxy cache projects for OCI (upstream chosen from an endpoint; pull through the project prefix; no push; speed limit to upstream; default 7-day retention for new proxy projects) | yes (npl): Docker proxy repositories | yes | `phase1` | OCI proxy repositories (REPO-02); bandwidth limit and default retention are `wave2` |
| `PRX-13` Proxy of Docker Hub and other registries | yes (npl) | yes: OCI registry, Docker Hub, generic registry, ECR, ACR, GCR, Quay, GHCR, Artifactory | `phase1` | generic OCI upstream with basic or bearer auth; vendor token exchange adapters `later` |
| `PRX-14` Registry endpoints as named, reusable upstream connections (type, URL, credentials, verify certificate, test connection) | no (per-proxy settings only) | yes | `wave2` | `/api/v1/registries`; proxy repositories, replication rules and scanners reference an endpoint by id |
| `PRX-15` Replication rules: push or pull; filters by name, tag, label, resource type; destination namespace; flatten levels; override; bandwidth limit; delete remote | no | yes | `wave2` | `rustryx-replication` (new), `/api/v1/replication-rules`; OCI first |
| `PRX-16` Replication triggers: manual, scheduled, event based (push, retag, delete); one active run per rule | no | yes | `wave2` | scheduler trigger plus an event bus on component events; event-based skips signed artifacts until the referrer is present |
| `PRX-17` Replication executions with task list, per-task log, stop; rule edit blocked while running | no | yes | `wave2` | `/api/v1/replication-executions` with tasks and logs |
| `PRX-18` Copy blobs in chunks (peer to peer only) | no | yes | `later` | optimization; plain monolithic or chunked blob upload first |
| `PRX-19` Replicate signatures and SBOMs together with the artifact | no | yes (peer to peer keeps the link) | `wave2` | copy referrers with the subject; document the limit for third-party targets |
| `PRX-20` Replication provider adapters (OCI registry, Docker Hub, ECR, ACR, GCR and Artifact Registry, GHCR, Quay, GitLab Registry, Artifactory, Artifact Hub pull only, Alibaba, Huawei, Tencent, VolcEngine) | no | yes | `wave2` | generic OCI distribution first; vendor adapters other than OCI auth are `later`; Alibaba, Huawei, Tencent and VolcEngine adapters are `out` (vendor specific, no compat parity) |
| `PRX-21` Replication between different product versions is not supported | no | yes (documented limitation) | `wave2` | version the replication protocol and reject unknown versions |
| `PRX-22` Content replication for non-OCI formats: hosted source, proxy target pre-fetches new components on a one-minute check; service account for auth; PostgreSQL required | yes (Pro): Apt, Docker, Conan, Helm, Maven, npm, NuGetV3, R, Raw, RubyGems, Cargo | no | `later` | after OCI replication; the same engine over a component change feed; ТЗ phase 2 lists content replication |
| `PRX-23` Replication is not a disaster recovery mechanism (documented caveat: no configuration copied, cleanup must be managed on the target) | yes (Pro) | no | `wave2` | document the same caveat in the runbook |

Sources: N-types, N-fields, N-http, N-routing, N-repl, N-docker, N-matrix, H-proxycache, H-repl, H-replendpoints, H-replrules, H-replmanage, H-compat, H-install.

## 9. Vulnerability scanning, CVE allowlists, signing and content trust, SBOM

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `SCN-01` Built-in image scanner (Trivy, optional component at install time) | no: health check relies on a vendor data service | yes | `wave2` | `rustryx-scan` (new) with a `Scanner` trait; Trivy adapter for docker and oci (01-architecture phase 2) |
| `SCN-03` Pluggable external scanners: register by name and endpoint, auth none, basic, bearer or API key, skip certificate verification option, test connection, several instances, default scanner, per-project scanner | no (IQ Server integration is separate) | yes | `wave2` | `/api/v1/scanners`; HTTP adapter contract defined by Rustryx; the peer's own adapter contract is `unverified` in the pages read, so interoperability is not promised |
| `SCN-04` Scan on push, enabled per project | no (hourly health check task for proxies) | yes | `wave2` | project and repository setting `auto_scan`; queue with `SKIP LOCKED` leases (01-architecture 1.3) |
| `SCN-05` Manual scan of one or several artifacts; scan all; scheduled scan all (none, hourly, daily, weekly, cron); stop scan and stop scan all | no | yes | `wave2` | `POST /api/v1/scans`, `/api/v1/scans/actions/scan-all`, schedule as a task |
| `SCN-06` Scan status per artifact (not scanned, queued, scanning, complete, failed with log) | no | yes | `wave2` | `scan_status` on the component; log link per scan |
| `SCN-07` Scan summary: highest severity colour, counts per severity, fixable count | yes (CE): health check summary counts critical, severe, moderate | yes | `wave2` | `GET /api/v1/components/{id}/vulnerabilities/summary` |
| `SCN-08` Detailed per-CVE report with CVSS and descriptions | yes (Pro): detailed report | yes: full list of the last scan, sortable and filterable | `wave2` | `GET /api/v1/components/{id}/vulnerabilities`; column set (CVE id, package, installed, fixed, severity, score) is my design, the peer's column list was `unverified` |
| `SCN-09` Image index scanning: each referenced image scanned, index result aggregates (success, limited, failed) | no | yes | `wave2` | aggregate rule documented in the scanner contract |
| `SCN-10` Export vulnerabilities to CSV per project with filters (repository, tag, label, CVE id), file removed after download | no | yes | `wave2` | async export job; `POST /api/v1/vulnerabilities/exports`, download once |
| `SCN-11` Security Hub: registry-wide totals by severity, top 5 artifacts, top 5 CVEs, search by CVE id with filters (severity, score range, project, repository, digest, package, tag) | no | yes: admin only | `wave2` | `GET /api/v1/vulnerabilities?cve=` over a `findings` table with indexes; CVE id mandatory for performance, as the OCI peer does |
| `SCN-12` System-wide CVE allowlist with optional expiry date | no | yes | `wave2` | `/api/v1/cve-allowlists` with `scope=system`; every entry audited |
| `SCN-13` Per-project CVE allowlist overriding the system list, copy from system, expiry | no | yes | `wave2` | `scope=project`; a copy does not track later system removals (same behaviour documented by the OCI peer) |
| `SCN-14` Pull gate: block pulls of artifacts with findings at or above a severity; checked at pull time per artifact; indexes judged by their children; negligible never blocks | yes (Pro+): Firewall policies | yes: "prevent vulnerable images" | `wave2` | ТЗ phase 2 pull gate; evaluated in the OCI pull path with a cached verdict keyed by digest and allowlist version |
| `SCN-15` Repository health check for proxies: vulnerability and license risk summary for Maven, npm, NuGet, PyPI, RubyGems, Yum; hourly check, report at most daily | yes (CE): summary; Pro: detailed | no | `wave2` | vulnerability part from the OSV and Trivy findings (section 11.7); the license risk part is `later` because it needs a license data source |
| `SCN-16` Component age and popularity indicators from the health check | yes (CE) | no | `later` | needs external popularity data; see `SRCH-13` |
| `SCN-17` Malware detection, quarantine and removal for proxies; dashboard; task | yes (Pro+): needs the Firewall add-on | no | `later` | needs a threat intelligence source and the pull gate; not in the Community feature set |
| `SCN-18` Namespace confusion protection and release integrity checks | yes (Pro+) | no | `later` | the group name-claiming rule (`REPO-46`) covers the main case; the rest needs Firewall-style data |
| `SCN-19` IQ Server integration (component intelligence, application analysis by email report) | yes (npl): needs the separate IQ Server | no | `out` | proprietary external service |
| `SCN-20` Offline vulnerability data for isolated sites (import of scanner data) | no | yes: a page about importing vulnerability data to an offline instance exists, content not read (`unverified`) | `wave2` | offline mirror of the Trivy database and OSV dumps; essential for sites without Internet and for the SSRF egress rules |
| `SCN-21` Scanner must not refresh the artifact pull time (setting) | no | yes: "retain image last pull time on scanning" | `wave2` | scanner reads go through a path that does not touch the pull-time counters (`BLOB-20`) |
| `SCN-22` Cosign and Notation signatures stored as accessories linked to the signed artifact; shown in an accessories table; deleting the artifact deletes them | no | yes | `phase1` | OCI Referrers API and tag-schema fallback (iteration 3); storage and linking are phase 1, UI and lifecycle coupling `wave2` |
| `SCN-23` Retention, immutability and replication apply to an artifact and its signatures together; signatures are not scanned | no | yes | `wave2` | lifecycle jobs treat referrers as part of the subject |
| `SCN-24` Content trust per project: block pulls of unsigned artifacts; Cosign, Notation or both; both enabled means both must verify | no | yes | `wave2` | `/api/v1/trust-policies` with trust material; see section 11.10; a presence-only check is not accepted |
| `SCN-25` Notation trust policy (trusted signers, verification level) and Cosign key or keyless identity as policy input | no | Notation: yes, trust policy created by the client; verification inside the OCI peer not explained in the pages read (`unverified`) | `wave2` | the server needs its own verification library; selection needs a spike (not verified) |
| `SCN-26` Server-side signing (APT hosted metadata signed with a GPG key) | yes (npl): APT hosted repositories | no | `wave2` | with the APT crate (`REPO-22`); key as a secret reference |
| `SCN-27` SBOM generation (SPDX through the default scanner), automatic on push per project, manual from the artifact list, stop | no | yes | `wave2` | `rustryx-scan` SBOM job; stored as an OCI accessory; format SPDX JSON first |
| `SCN-28` SBOM display (package names, versions, licenses) and download of the full file | no | yes | `wave2` | `GET /api/v1/components/{id}/sbom` |
| `SCN-29` SBOM replicated with the artifact; SBOM deleted with the subject or alone, physical removal at GC | no | yes | `wave2` | same accessory rules as signatures (`SCN-23`) |
| `SCN-30` CycloneDX format | no | not seen (only SPDX described) | `later` | not requested by either product's docs read; add when a consumer needs it |

Sources: N-rhc, N-malware, N-compinfo, N-matrix, N-fields, H-scan, H-pluggable, H-sysallow, H-schedulescans, H-scanall, H-deploysec, H-scanone, H-hub, H-sbom, H-images, H-contenttrust, H-projallow.

## 10. Webhooks, audit, observability, system configuration, HA, API surface, CLI

| Capability | compat Repository | OCI peer | Rustryx target | Rustryx implementation note |
|---|---|---|---|---|
| `OPS-01` Webhooks: global and per-repository targets, selectable event types, optional secret signing | yes (Pro+): webhook capabilities, listed under the Firewall-licensed items | yes: per project | `wave2` | `rustryx-notify` (new), `/api/v1/webhooks`; HMAC-SHA256 with timestamp, per-webhook secret, replay window (SEC-38); SSRF filter on targets |
| `OPS-02` Webhook signature header: HMAC SHA-1 of the compact JSON body in a signature header | yes (Pro+) | no signature described: an auth header is configured instead | `wave2` | Rustryx signs with SHA-256 and a timestamp; document a verification snippet for consumers |
| `OPS-03` Webhook endpoint kinds: HTTP with default or CloudEvents payload, Slack | no | yes | `wave2` | HTTP JSON and CloudEvents first; Slack formatting is `later` (payload only) |
| `OPS-04` Webhook event catalog: push, pull, delete, scan completed, scan failed, scan stopped, quota exceeded, quota near threshold, replication status, tag retention finished | no | yes | `wave2` | typed event enum in `rustryx-core`; global `audit` stream from compat maps to the audit sink |
| `OPS-05` Webhook enable and disable per webhook; system-wide on or off; retry count for delivery jobs | no | yes | `wave2` | delivery log table with retries and backoff, `/api/v1/webhook-deliveries` |
| `OPS-06` Email server settings (SMTP) with verify and disable; used for task notifications and links | yes (npl): email API with get, set, disable, verify | the OCI-peer docs read did not cover email settings (`unverified`) | `wave2` | `/api/v1/settings/email` plus `POST .../verify`; password as a secret reference |
| `OPS-07` Task result email (failure only or success and failure) | yes (npl) | no | `wave2` | tied to `RET-17`; opt-in per task |
| `OPS-08` Base URL setting used in emails and generated links | yes (npl): capability | external URL in the install config | `phase1` | `http.base_url` is mandatory config (SRE-35) |
| `OPS-09` Audit log with domains security, repositories, system, firewall; event types created, updated, deleted, purged, changed; entry has time, node, initiator, domain, type, context, attributes | yes (npl): JSON lines file, daily rotation, 90 days | yes: stored in the database | `phase1` | audit of mutations and logins, NDJSON export, stdout JSON (README 3.1, 8.6); monthly partitions (01-architecture 1.3) |
| `OPS-10` Audit viewer with filters (operation type, user, resource, resource type, date) | no viewer seen (file only) | yes | `wave2` | `GET /api/v1/audit-events` with the same filters; export stays NDJSON |
| `OPS-11` Audit event forwarding to a syslog endpoint; option to skip the database copy; per-event-type disabling | no | yes | `phase1` | stdout JSON collected by an agent is the phase 1 path; syslog over TLS and per-type toggles are `wave2` (SEC audit section) |
| `OPS-12` Project-scoped log view for members (admins, maintainers, developers, guests) | no | yes | `wave2` | `GET /api/v1/audit-events?project=` with a project privilege check |
| `OPS-13` Application log viewer in the UI (admin, not available in HA) | yes (npl) | no | `out` | logs go to stdout and the collector; an in-UI viewer cannot work across stateless nodes |
| `OPS-14` Change logger levels at runtime without restart | yes (npl) | log level in config | `wave2` | `PUT /api/v1/system/loggers/{name}` through a `tracing` reload handle, per node |
| `OPS-15` Request log, outbound request log, task logs | yes (npl) | log settings: local rotation and retention, optional external syslog | `phase1` | structured access log with route templates (SRE-08); task logs per run in `wave2` |
| `OPS-16` Prometheus metrics endpoint with privilege or basic auth | yes (npl): privilege `nx-metrics-all` | yes: separate port, components core, registry, jobservice, exporter | `phase1` | `/metrics` bearer-protected by default (SRE-08) |
| `OPS-17` Distributed tracing (OTLP, Jaeger), one exporter, request id correlation | no | yes: OpenTelemetry since 2.4 | `phase1` | OTLP (SRE-12); `X-Request-ID` correlation |
| `OPS-18` Health and readiness endpoints | yes (npl): status page and health endpoints; REST status page not read | metrics `registry_health`, `registry_up`; CLI health command | `phase1` | `/healthz` and `/readyz` per README 8.2 |
| `OPS-19` Status page with checks: resources, blob stores, license, default admin password, scripting, update available, read-only mode, retry storms, H2 limits | yes (npl) | no | `wave2` | `GET /api/v1/system/status`: list of checks with severity, e.g. default credentials, anonymous enabled, tokens that never expire, blob store quota |
| `OPS-20` System information download (version, plugins, paths, JVM and OS properties, environment variables) per node | yes (npl) | CLI info command | `wave2` | `GET /api/v1/system/info` redacted; no raw environment dump because env may hold secrets |
| `OPS-21` Support ZIP (logs and sanitized configuration, per node in HA) | yes (npl) | no | `later` | a sanitized `rustryx support-bundle` command; the secret-leak review comes first |
| `OPS-22` Read-only mode: pulls allowed, pushes and deletes blocked | yes (npl): earlier versions; removed in 3.91 and 3.92 | yes | `wave2` | system flag stored in PostgreSQL, checked by write middleware; required for consistent backups |
| `OPS-23` Banner message (type, closable, duration) and branding header or footer | yes (npl): branding and UI settings | yes: banner | `later` | UI polish; banner text only, no custom HTML (XSS) |
| `OPS-24` Global project creation policy (everyone or admins only) | no | yes | `wave2` | `settings.projects.creation` with projects (section 11.1) |
| `OPS-25` Capabilities framework (typed feature settings created in an admin list) | yes (npl) | no | `out` | not copied as a generic framework; each capability that matters maps to a typed setting (section 12.4) |
| `OPS-26` HA with several nodes behind a load balancer, shared blob store, shared database | yes (Pro) | yes: stateless components, external PostgreSQL and Redis, shared or object storage | `phase1` | README 3.1 and REQ-07; no sticky sessions |
| `OPS-27` Node list and status in HA | yes (Pro) | no | `wave2` | `GET /api/v1/system/nodes` from heartbeat rows |
| `OPS-28` Database: embedded H2 or PostgreSQL with pool settings; connection info screen | yes (npl): H2 for development only | external PostgreSQL option | `phase1` | PostgreSQL only; H2 is `out` (embedded databases do not fit stateless nodes) |
| `OPS-29` Usage center: requests per day, component totals, Community cap | yes (npl) | no | `later` | Prometheus based usage dashboard; Rustryx has no licensing caps |
| `OPS-30` License management and EULA API | yes (npl) | no | `out` | MIT; nothing to manage |
| `OPS-31` Telemetry page | yes (npl): page exists, content not read | no | `out` | no phone-home by design |
| `OPS-32` Switch between classic and new UI | yes (npl) | no | `out` | one UI only |
| `OPS-33` compat v1 REST API: repositories, components, assets, search, security (users, roles, privileges) | yes (CE): REST API | no | `phase1` | README 3.1, `rustryx-api` under `/service/rest/v1/` |
| `OPS-34` compat v1 REST API: blob stores, cleanup policies, status | yes (CE) | no | `phase1` | 01-architecture phase 1 table |
| `OPS-35` compat v1 REST API: tasks, capabilities, email, HTTP configuration, logging, nodes, support, user tokens, tagging, staging, firewall, lifecycle, licensing, metrics data | yes (npl): API reference lists them | no | `wave2` | implement only the ones whose native feature exists (tasks, email, http, logging, nodes, user tokens, tags, staging); capabilities, licensing, firewall and lifecycle are `out` |
| `OPS-36` Script REST API (Groovy) | yes (npl): disabled by default | no | `out` | see `AUTH-47` |
| `OPS-37` Native REST API with interactive explorer (Swagger UI), API version 2.0; authorize with basic credentials | yes (npl): Swagger UI needs a settings-read privilege | yes | `wave2` | OpenAPI contract in `docs/openapi/rustryx-api-v1.yaml` (the source for the generated TypeScript client, see `ui-design-brief.md`), served at `/api/v1/openapi.json`, plus an explorer page (UI inventory) |
| `OPS-38` Pagination convention | yes (npl): continuation token | `unverified` | `phase1` | `continuationToken` (01-architecture 1.6) |
| `OPS-39` Official command line client | no (REST and package clients only) | yes: a registry CLI (projects, repositories, artifacts, labels, tags, quotas, webhooks, health, info) | `wave2` | `rustryx-cli` (new), binary `ryx`, OIDC device flow for the confidential `rustryx-cli` client (README 5); mirrors `/api/v1/` |
| `OPS-40` Bundles and plugin development (new format support) | yes (npl): bundle development pages | no | `out` | formats are compiled in, no plugins (ADR 11) |

Sources: N-webhooks, N-email, N-audit, N-logging, N-prom, N-status, N-sysinfo, N-support, N-caps, N-nodes, N-datastore, N-usage, N-auto, N-apiref, N-oneui, N-matrix, N-pro, H-audit, H-settings, H-metrics, H-tracing, H-api, H-projcfg, H-install, H-cli.

## 11. OCI-registry concepts that need a design decision

Each subsection states what the OCI-registry documentation describes (in own words), the proposed Rustryx decision, and the entities and native API resources involved. Statements about the OCI peer that the pages did not confirm are marked `unverified`.

### 11.1 Projects (namespaces over repositories, with members and roles)

- OCI peer: a project is the namespace in an image path (`project/repository:tag`). It is public or private, has members with one of five roles (limited guest, guest, developer, maintainer, project admin), and carries its own configuration: auto scan, pull gate, CVE allowlist, content trust, retention, immutability, webhooks, robot accounts, labels, scanner choice, logs and a storage quota. Creating projects can be limited to system admins.
- Rustryx decision: a `project` is an optional access-and-policy container over repositories of any format, not an OCI-only construct, because Rustryx repositories are compat-style and format typed. A repository belongs to at most one project (`repositories.project_id`, nullable). Repositories without a project keep the compat behaviour (global admin, privileges and selectors). For OCI the default mapping is one hosted OCI repository named like the project, so `docker push host/payments/api:1.0` reaches `/v2/payments/api` and needs no new URL space.
- Roles: the five project roles are templates that expand into ordinary privileges (repository view actions plus a few project actions) over every repository of the project. Membership rows reference a user or an identity-provider group (for example the Keycloak path `/rustryx/team-payments/publishers` mapped to developer in project `payments`, which matches the example group layout in README section 5). Expansion happens in `rustryx-rbac`, so there is still exactly one authorization engine.
- Visibility: public sets `allow_anonymous` read on the project repositories, and still needs the global anonymous switch (README 8.5, 01-architecture 1.5). Anonymous write is never granted.
- Boundaries: a project admin edits members and project policies only; registries, scanners, the system CVE allowlist, quotas and global settings stay with `nx-admin` (matches the peer permission matrix where quota edit is system admin only).
- Resources: `/api/v1/projects`, `/api/v1/projects/{id}/members`, `/api/v1/projects/{id}/repositories`, `/api/v1/projects/{id}/policies`. compat v1 compat endpoints do not show projects.
- Open points: project deletion (block while repositories are non-empty, or cascade after a typed confirmation); how a project with several formats shows quota (sum over repositories).

### 11.2 Robot accounts (mapped onto API tokens with scopes)

- OCI peer: system robots (all projects including future ones, or a chosen set with per-project permissions) and project robots; secret shown once; expiry default 30 days, "never" possible; refresh the secret with a random or manual value; disable without deleting; name prefix; push requires pull; permissions cannot exceed the creator's; robots cannot sign in to the UI.
- compat: service account tokens bound to one role, expiry 30, 60, 90 days, a year or never, shown once, revocable, expired ones purged daily (Pro, 3.97.0).
- Rustryx decision: no separate account table with a password. A robot is an `api_tokens` row (`ryx_` prefix, SHA-256 stored) with `kind=robot`, optional `project`, `scopes[]` (pairs of resource selector and action), `expires_at`, `disabled`, `created_by`, `last_used_at`. The principal gets privileges only from its scopes. At creation the scopes must be a subset of the creator's effective privileges. Role presets in the UI expand to scopes, which covers the compat role-bound style.
- Registry login: Docker and Helm use the token name as username and the token as password. The token name pattern is `robot+<project>+<name>` and is configurable.
- Rules: expiry mandatory by default with a configurable maximum; never-expire needs `nx-admin` and a setting; rotation invalidates the old secret at once; session login routes reject robot principals; every create, rotate, disable, delete and last-use is audited.
- Resources: `/api/v1/api-tokens?kind=robot&project=`, `POST /api/v1/api-tokens/{id}/actions/rotate`, project alias `/api/v1/projects/{id}/robots`.

### 11.3 Retention policies

- OCI peer: tag retention keeps what matches rule templates (N most recently pushed, N most recently pulled, pushed within N days, pulled within N days), with repository and tag selectors in matching or excluding mode and an option for untagged artifacts. Several rules (up to 15) combine with OR. Evaluation is by tag but action is by artifact, so an artifact with any kept tag is kept. One schedule per project, dry run and run now.
- compat: cleanup policies with AND-ed criteria (age, last download, release type, retained versions, name matcher) per format, previewed and executed as a soft delete.
- Rustryx decision: one lifecycle engine in `rustryx-tasks` with two policy modes. `cleanup` mode (compat style) deletes what matches all criteria. `retain` mode deletes what is in scope and not kept by any rule. Both first produce a plan (candidates plus the reason each was chosen) stored as a run with `dry_run=true`, and a real run re-evaluates and applies in batches as a soft delete. Mandatory protections: immutability rules (11.4), release repositories and legal hold (DSO-34), and never removing the last remaining version unless the policy says so explicitly.
- Deletions are audited in batches that list the removed digests or coordinates, so volume stays bounded and DSO-34 is still met.
- Resources: `/api/v1/cleanup-policies`, `/api/v1/retention-policies`, `/api/v1/lifecycle-runs` (plans and reports, CSV export).
- Dependencies: `last_downloaded` and pulled templates need the pull counters (`BLOB-20`). Proxy repositories get a recommended 7-day retention template (the OCI peer applies one by default to new proxy cache projects), offered as a choice and not forced.

### 11.4 Immutability rules

- OCI peer: per-project rules with repository and tag selectors; a tag matched by any rule cannot be overwritten, deleted, re-tagged or replaced by replication; other tags of the same artifact stay editable; up to 15 rules. How this interacts with retention and proxy cache was `unverified` in the pages read.
- Rustryx decision: `immutability_rules` over a scope (project or repository) with a selector (OCI: repository and tag patterns; other formats: the content selector expression on path or coordinates). Matching writes are rejected with the OCI `DENIED` error or HTTP 409. Re-pushing the identical digest is allowed (idempotent publish; OCI-peer behaviour for this case is `unverified`). It complements the repository write policy (`allow`, `allow_once`, `deny`).
- Interaction decision (conservative): cleanup, retention, promotion move and replication skip immutable items unless an `nx-admin` sets `override_immutable` on that run. The check runs inside the same transaction that publishes or deletes the tag, so a race cannot slip a change past it.
- Resources: `/api/v1/immutability-rules`; events `IMMUTABLE_REJECT` in the audit stream.

### 11.5 Quotas

- OCI peer: storage quota per project in MiB, GiB or TiB, global default for new projects, per-project override, minus one means unlimited. Blobs shared between projects count once per project, so totals can exceed disk. Retagging into another project adds usage. The manifest is pushed last, so a push can pass the first check and still fail; concurrent pushes compete first come first served. Garbage collection lowers usage.
- compat: soft quota per blob store (alert only; by maximum used or minimum free space).
- Rustryx decision: two layers. (1) Hard quota on a project, optionally on a repository (`quota_bytes` already appears in 01-architecture 1.3): distinct blobs referenced by the scope, kept in `quota_usage` and updated in the publish and delete transactions, with a recount task as a safety net. A pre-check uses the declared length and rejects early; the authoritative check runs when the manifest or component is committed. Reads and deletes always continue, as in the disk-full rule SRE-34. (2) Soft quota on a blob store (`BLOB-12`), evaluated by a periodic job, never on the write path.
- Events: quota exceeded and near-threshold feed the webhook catalog. Quota usage is visible to every project member; only `nx-admin` edits limits.
- Resources: `/api/v1/quotas`, `GET /api/v1/projects/{id}/quota`.

### 11.6 Replication rules and registry endpoints

- OCI peer: registry endpoints (provider, URL, credentials, verify certificate, test connection) and rules (push or pull, filters on name, tag, label and resource type, destination namespace, flatten levels, override, bandwidth, delete remote for event-based) with manual, scheduled or event triggers, executions with per-task logs, retries after a few minutes, one active run per rule, no replication between different peer versions.
- Rustryx decision: `registries` (provider `rustryx` or generic `oci` first; URL, credential reference, CA reference, last test result) are shared by proxy repositories, replication rules and any future adapter. Rules, filters and triggers follow the OCI-peer model. The executor runs under a distributed lock per rule and creates retryable per-artifact tasks in the lease queue; it copies manifests and blobs, then referrers (signatures and SBOMs) after the subject. The URL passes the SSRF filter (README 8.6). A protocol version header lets nodes refuse an incompatible peer.
- Out of the first cut: vendor token-exchange adapters, compat-style pre-emptive pull for non-OCI formats (`PRX-22`).
- Resources: `/api/v1/registries`, `/api/v1/replication-rules`, `/api/v1/replication-executions`, `.../tasks`.

### 11.7 Scanner integration (Trivy and OSV)

- OCI peer: Trivy is the default scanner (optional at install), other scanners attach through an endpoint with none, basic, bearer or API key authentication, one default and a per-project choice; scan on push, manual, scan all, scheduled, stop; findings summary and per-CVE list; Security Hub; CVE export; SBOM generation by the default scanner.
- Rustryx decision: `rustryx-scan` (new crate) defines a `Scanner` trait with `capabilities` (vulnerability, sbom), `scan` and `health`. Adapters: `trivy` for OCI images (Trivy server mode or a resource-limited job with an offline database; arguments never built from untrusted strings), `osv` for maven, npm, pypi and cargo using package URLs from stored component metadata (no artifact download needed), and a generic `http` adapter for external scanners with a Rustryx-defined contract.
- Data: `scan_reports` (component, scanner, status, timing, summary) and `findings` (report, CVE id, package, installed and fixed version, severity, score) with an index on CVE id for the hub search. Allowlist and pull-gate evaluation are pure functions over findings so they are unit-testable.
- Triggers: after the publish commit (never blocking the push), manual, scheduled, and a periodic re-evaluation of stored package URLs against updated OSV data, which finds new advisories without a rescan of bytes.
- Rules: a concurrency semaphore and per-scan timeout; scans never refresh pull time (`SCN-21`); offline mirrors for isolated sites (`SCN-20`); the online OSV mode is off by default because it sends package coordinates to a third party.
- Resources: `/api/v1/scanners`, `/api/v1/scans`, `/api/v1/vulnerabilities`, `/api/v1/cve-allowlists`.

### 11.8 Webhooks

- OCI peer: per-project webhooks with HTTP (default or CloudEvents payload) and Slack kinds, an optional auth header, a catalog of events (push, pull, delete, scan completed, failed and stopped, quota exceeded and near threshold, replication, retention finished), per-webhook enable, a system switch, and a retry limit.
- compat: global and repository webhook capabilities, HMAC SHA-1 signature header.
- Rustryx decision: `rustryx-notify` (new crate). Events are written to an outbox table in the same transaction as the state change, then a dispatcher under a lease delivers them at least once with exponential backoff and a configurable cap. Each delivery has a row (attempt, status code, latency, truncated response). Signing uses HMAC-SHA256 over timestamp plus body with a per-webhook secret (SEC-38, replay window 5 minutes). Targets pass the SSRF filter, redirects are not followed, timeouts and response sizes are capped, and the host allowlist is empty by default (SEC section on SSRF).
- Scopes: system, project, repository. Pull events are optional because of volume.
- Resources: `/api/v1/webhooks`, `/api/v1/webhook-deliveries`, `POST /api/v1/webhooks/{id}/actions/test`.

### 11.9 Labels (and the relation to compat tags)

- OCI peer: global labels (system admins) and project labels (project admins) applied to artifacts by developers and above, filterable in lists, listed first for global then project.
- compat: tags (Pro) group components under a name such as a build id; REST CRUD; name up to 256 characters of letters, digits, underscore, hyphen and dot; JSON attributes up to 20k; used by promotion and the tag cleanup task.
- Rustryx decision: one metadata mechanism named `labels` with `scope` (system or project), `color`, `description`, bounded `attributes` and a `component_labels` join. The compat tag REST resource is a compatibility view of system-scope labels. In API and UI the word "tag" is reserved for OCI tags to avoid the clash between an OCI tag (version pointer) and a compat tag (label).
- Uses: list and search filters, replication filter, promotion by label, tag cleanup task.
- Resources: `/api/v1/labels`, `/api/v1/components/{id}/labels`, compat `/service/rest/v1/tags`.

### 11.10 Content trust (Cosign and Notation verification policy)

- OCI peer: signatures are stored as accessories linked to the signed artifact and shown in an accessories table; retention, immutability and replication treat artifact and signatures together; a project policy can require Cosign, Notation or both before an artifact can be pulled; Notation needs a trust policy created on the client side; how that product itself verifies was not explained in the pages read (`unverified`).
- Rustryx decision: `trust_policies` per project or repository with `required` set to none, cosign, notation or both, and trust material: for Cosign a list of public keys or keyless identities (issuer and subject patterns); for Notation a trust store and trust policy (trusted identities, verification level). Signatures are found through the OCI Referrers API that phase 1 already provides. The verdict is cached by subject digest, policy version and signature digest. A pull of an unverified artifact is denied with a reason. A check that only tests whether a signature exists is rejected as a design: it would give a false sense of security.
- Indexes: the policy verifies what was requested; a `verify_children` option covers multi-arch indexes (OCI peer's behaviour for indexes in this area is `unverified`).
- Risks: the verification library is not chosen. A spike must pick a maintained Rust implementation for each scheme, or run the official verifier as a sandboxed helper process. This is explicitly not verified in this research.
- Resources: `/api/v1/trust-policies`, `GET /api/v1/components/{id}/accessories`.

### 11.11 Proxy cache projects

- OCI peer: a proxy cache project points at an upstream endpoint; users pull through the project prefix; cached images are revalidated with a lightweight HEAD; if the upstream changed, the new image is fetched; if the upstream is down, the cached copy is served; no push; the upstream account decides what is reachable; optional pull speed limit; new proxy projects get a 7-day retention policy by default.
- Rustryx decision: not a separate project type. It is an OCI repository of type proxy that belongs to a project and references a registry endpoint. Adopt the OCI peer freshness rule (manifest HEAD and digest compare) and serve-stale with a marker header (iteration 2). Because the upstream account defines exposure, each OCI proxy gets an optional namespace allowlist (for example `library/*`), shown as a warning in the UI when empty. Add a bandwidth limit (minus one for none). Retention template suggestion as in 11.3. Quota behaviour for proxy content is `unverified` in OCI peer, so Rustryx counts cached blobs in the project quota and relies on retention to evict.
- Resources: `/api/v1/repositories` with `type=proxy`, `/api/v1/registries`.

### 11.12 Garbage collection with dry run

- OCI peer: GC removes blobs no manifest references; manual run (not more than once a minute), schedule (hourly, daily, weekly, cron, none), dry run listing what would go and the estimated space, an option to delete untagged artifacts first, a worker count, a two-hour safety window for fresh uploads, a history table with logs, and stop. It runs without pausing pushes and pulls.
- Rustryx decision: two phases. Deletion by users or policies is a soft delete (tombstone). The GC job marks blobs that are unreferenced and older than `gc.min_age` (default 24 hours per 01-architecture, longer than OCI peer's two hours because several nodes stage uploads) and sweeps them in batches, re-checking references inside a transaction per blob so a new reference created since the mark wins. Dry run executes only the mark phase and stores a report (counts, bytes, per blob store, capped sample list). Options: `delete_untagged` (OCI manifests untagged for longer than `min_age` and not referenced as subject or by an index), bounded workers, history with trigger, status, log link and cooperative stop (already removed blobs are not restored), manual trigger rate limit of one per minute.
- HA and monitoring: lock `rustryx:lock:gc:<store>`; metric `background_job_last_success_timestamp_seconds{job="blob_gc"}` and the stale alert (SRE section). Abandoned multipart and Docker uploads are removed by the same job.
- Resources: `/api/v1/gc/runs`, `POST /api/v1/gc/runs` with `dry_run`, `/api/v1/tasks` for the schedule.

## 12. compat concepts that need a design decision

### 12.1 Repository health

- compat: the health check covers proxy repositories of Maven, npm, NuGet, PyPI, RubyGems and Yum. The summary (all editions) counts components by vulnerability severity and by license category; the detailed report with CVSS scores and licenses is Pro. It relies on compat vendor data services, runs an hourly check and refreshes a report at most daily, and is enabled per repository or for all supported repositories.
- Rustryx decision: "repository health" is an aggregate over stored scan findings (11.7), not a separate subsystem. The repository list shows a severity histogram per repository, and the repository page shows a card with the top findings. Enabling is the repository `health_check` flag. License categories are `later` because Rustryx has no license data source of its own. No call goes to a vendor service; OSV online mode is opt-in (`osv.mode = offline` by default).
- Resources: `GET /api/v1/repositories/{name}/health`, `PUT .../settings` for the flag.

### 12.2 Cleanup policies

- compat: admins create policies with a name, a format and criteria that differ per format; repository editors attach several policies to a repository; a preview shows the point-in-time result; execution is a scheduled task that soft deletes, and a compaction step later frees disk space.
- Rustryx decision: same model on top of the shared lifecycle engine (11.3, 11.12). Each `FormatHandler` declares the criteria it supports, for example Maven: age, last download, release type, kept versions, name regex; npm, PyPI, Helm, raw and pub: age, last download, kept versions, name; OCI: age, last download, kept versions, tag regex (no release type, same as the compat Docker case). The preview is a dry-run lifecycle run with a CSV export that is stored and not limited to a one-minute request.
- Resources: `/api/v1/cleanup-policies`, `/api/v1/lifecycle-runs`.

### 12.3 Tasks (compact blob store, rebuild index and the rest)

- compat: typed tasks created by admins, with frequencies manual, once, hourly, daily, weekly, monthly or cron; enable, disable, status, last result; optional email on failure or on every run; logs kept 30 days.
- Rustryx decision: a typed task registry in `rustryx-tasks`. Tables: `task_definitions` (kind, parameters, schedule, enabled, notify) and `task_runs` (status, node, start, end, summary, dry_run, log pointer). System tasks (audit partition rotation, token purge, download counter flush, GC) appear read-only with an editable schedule. Every run of a singleton kind takes a distributed lock. Run now and stop are cooperative (01-architecture 1.4).
- Mapping of compat task types: compact blob store, cleanup unused asset blobs, delete temp files, Docker incomplete uploads and unused layers all become the GC job; rebuild search index and browse tree repair tasks are `out` (search and tree live in PostgreSQL); delete SNAPSHOTs becomes a Maven cleanup template; backup H2 is `out`; repair and reconcile become `verify-blobs`; replicate content becomes replication (11.6); import and export assets become `export-repository` and `import-repository`; cleanup tags becomes `cleanup-labels`; Apt, Helm and Yum metadata rebuild become one `rebuild-metadata` kind; change repository blob store is `later`.
- Resources: `/api/v1/tasks`, `/api/v1/tasks/{id}/runs`, `POST .../actions/run` and `.../stop`.

### 12.4 Capabilities

- compat: a generic list of typed feature switches managed in one admin view: audit, base URL, branding, UI settings, default role, remote user token header, webhooks, health check, storage settings, scheduler, browse trim, node identity, custom S3 regions, Crowd, Firewall, license expiry, outreach, NXRM2-style URLs.
- Rustryx decision: no generic capability framework. Each capability that matters maps to a typed, schema-validated, audited setting section, and the rest are dropped:

| compat capability | Rustryx home |
|---|---|
| Audit | always on, `audit.*` config (stdout and retention) |
| Base URL | `http.base_url` (mandatory config) |
| Default role | `settings.auth.default_role` (`AUTH-24`) |
| Remote user token | auth provider `rut` (`AUTH-22`) |
| Webhook global and repository | webhooks (`OPS-01`) |
| Health check configuration | scanning settings (`SCN-15`) |
| Storage settings (last-downloaded interval) | `settings.storage.last_downloaded_interval` |
| Scheduler | tasks (12.3) |
| Browse trim | not needed: the tree is maintained on delete (12.7) |
| Custom S3 region | free-text `region` on the S3 blob store |
| Branding, UI settings | banner text only, later (`OPS-23`) |
| Node identity | node id from config or hostname, shown in `/api/v1/system/nodes` |
| Crowd, Firewall, license expiry, outreach, NXRM2 URLs | `out` |

- Resources: `/api/v1/settings/{section}` with a JSON schema per section served from `/api/v1/settings/schema`.

### 12.5 Routing rules

- compat: a rule has a mode (allow or block), a regular expression matched against the request path, and is assigned to a proxy repository (one rule per proxy, one rule shared by many). It limits what is pulled through the proxy even inside groups. The editor has a test box. A rule cannot be deleted while assigned. The page warns about slow regular expressions and denial of service.
- Rustryx decision: `routing_rules(name, mode, patterns[], description)`. Matching uses the path after the format's `classify` normalization (01-architecture 1.6.1), so Docker paths are treated consistently. Regular expressions use a linear-time engine with a size limit and no back-references. Rules are checked before the cache lookup, so a new block rule takes effect at once; compat does not state how it treats content already cached (`unverified`). Deleting an assigned rule returns 409. The test endpoint takes sample paths and returns the verdict.
- Resources: `/api/v1/routing-rules`, `POST /api/v1/routing-rules/{id}/test`.

### 12.6 Component upload

- compat: the UI uploads to hosted repositories only; the form is format specific with allowed file types; Maven and raw accept several files at once; Maven coordinates come from the form or from an uploaded POM (no POM is generated); formats such as Docker, OCI, Cargo and Conan cannot be uploaded in the UI; the REST API offers the same.
- Rustryx decision: `POST /api/v1/components?repository=` multipart with a JSON `attributes` part and one or more files, streamed (README 8.1) with the declared SHA-256 verified when supplied; response is 201 with component and assets. It calls the same publish use case as the native protocol, so write policy, selectors, immutability, quota, audit and webhooks apply exactly once. The UI builds the form from `GET /api/v1/formats/{format}/upload-schema` (fields, accepted extensions, multi-file flag), so format knowledge stays in the format crates. Write on the repository is the only privilege needed. Labels can be attached at upload (`UPL-07`).

### 12.7 Browse tree

- compat: a searchable tree of repository contents with up to 10,000 items per level, folders expand in place, empty folders are trimmed after deletes (a capability), and the item pane opens beside the tree.
- Rustryx decision: maintain `browse_nodes(repository_id, parent_path, name, kind, asset_id, child_count)` inside the publish and delete transactions (the delete path removes empty parents, which replaces compat's browse trim). The API returns one level: `GET /api/v1/repositories/{name}/tree?path=/org/&q=&limit=200&continuationToken=`, sorted folders first, with a server cap on page size and a virtualized list in the UI instead of a hard 10,000 cut.
- Authorization: with content selectors a folder is visible when some permitted asset lies under it. Selectors that are plain prefixes are checked by prefix containment; others are evaluated on the children of the page, with the cost bounded by SEC-18. Group repositories show the merge of members with first-wins on a path conflict; proxy repositories show cached content only (compat behaviour for proxies is `unverified`). OCI repositories use the component list (image name to tags), not a path tree.
- Resources: `/api/v1/repositories/{name}/tree`, `GET /api/v1/assets/{id}`.

### 12.8 Privileges and content selectors

- compat: privilege types repository view, repository admin, repository content selector, application, script, wildcard and uploader metadata read, with actions browse, read, add, edit, delete (application: create, read, update, delete). A selector is an expression over `format` and `path` plus format-specific fields (Maven, NuGet, Swift, Terraform), previewed with a cap of 10 results; the chain is selector, privilege, role, user or group. Selectors add access and never revoke.
- Rustryx decision: keep the ТЗ model (01-architecture 1.5): additive only, actions browse, read, write, delete. The compat compat layer maps add and edit to write and accepts `nx-repository-view-<format>-<repo>-<action>` names. Privilege kinds: `repository-view`, `content-selector`, `repository-admin`, `application` (wave 2) and `nx-admin`. Script privileges are `out`. Selectors use the CEL-like language with `format`, `path`, `repository` and `coordinate.*`.
- Parity gaps to close: preview endpoint (cap 10), parent-folder semantics for browse (12.7), per-area application privileges instead of the single admin gate, and a translator from compat CSEL expressions (`==`, `=^`, `=~`) to the Rustryx language for migration (`later`).
- Resources: `/api/v1/privileges`, `/api/v1/content-selectors`, `POST /api/v1/content-selectors/preview`.

### 12.9 Blob store groups and quotas

- compat: a group blob store spreads writes over members with a fill policy (round robin or write to first); converting an existing store into a group is irreversible; removing a member runs a task that moves its blobs to the group; soft quotas (maximum used or minimum free, in MB) only alert and appear in logs, the status page and a REST resource. Group stores are Pro.
- Rustryx decision: `blob_stores(kind: local, s3, azure or group)`; a group has ordered members and `fill_policy`. Each blob row stores the concrete member store, so reads never probe members. Writes skip members that are offline or over their soft quota. Groups cannot contain groups. "Convert to group" is implemented as: create the group with the existing store as first member and repoint the repositories in one transaction; blob rows are unchanged, which makes it safe even though it stays irreversible in the UI. Removing a member copies blobs to the remaining members with checksum verification, then updates `blobs.blob_store_id` (possible because keys are opaque, ADR-3).
- Soft quota: `quota_type` (max_used or min_free) and `quota_bytes`, evaluated by a periodic stats job that feeds a gauge, the status checks and an alert. It never blocks writes. Hard limits belong to project quotas (11.5).
- Resources: `/api/v1/blob-stores`, `/api/v1/blob-stores/{name}/members`, `GET /api/v1/blob-stores/{name}/usage`.

## 13. The 20 highest-value gaps relative to the phase 1 scope

Baseline: README section 3.1 and 8.6 (hosted, proxy and group repositories for seven formats, local and S3 blob stores, local, OIDC, LDAP and API-token authentication, roles, privileges and content selectors, the compat v1 REST API subset, audit, health, metrics, tracing, HA, cleanup policies, GC) and no web UI (README 3.2 and 01-architecture 1.1 keep the UI outside the Rust part). Size: S is a few days, M a few weeks, L more than a month for one engineer. Ordered by value for the stated requirement (compat Community plus OCI peer, with a quality UI).

| Rank | Gap | Why it ranks here | Capability IDs | Size | Stream |
|---|---|---|---|---|---|
| 1 | Web UI: shell, browse, search, upload, administration | the product owner requires a quality UI and phase 1 has none, while both reference products ship one | all of the UI inventory | L | UI-M1 |
| 2 | Native `/api/v1/` admin API with OpenAPI, browse tree, component and asset detail, audit query, system status, client-setup and upload-schema endpoints | the UI contract (01-architecture 1.1) is not yet defined by any phase 1 endpoint; every UI page depends on it | `SRCH-08`, `SRCH-10`, `SRCH-12`, `SRCH-14`, `OPS-10`, `OPS-19`, `OPS-37`, `UPL-01`, `AUTH-31`, `AUTH-53` | M | W2-A |
| 3 | Projects with members, project roles and Keycloak group mapping | OCI peer's central abstraction and the natural home for the group layout in README section 5 | `AUTH-42`, `AUTH-55`, `AUTH-56`, `AUTH-82`, `OPS-24` | L | W2-B |
| 4 | Robot and service tokens with scopes, expiry, rotation, project binding | CI access without personal accounts is the main automation need in both products; `ryx_` tokens exist but have no scoped admin model | `AUTH-62` to `AUTH-67` | M | W2-B |
| 5 | Vulnerability scanning core: Trivy for images, OSV for packages, results, summaries, scan on push and scheduled | OCI peer's most visible feature and a ТЗ phase 2 item; the compat health check is a weaker variant | `SCN-01` to `SCN-09`, `SCN-15`, `SCN-20` | L | W2-D |
| 6 | CVE allowlists, pull gate, Security Hub, CSV export | turns scan data into enforcement and into a dashboard | `SCN-10` to `SCN-14` | M | W2-D |
| 7 | Retention policies and tag immutability rules with dry-run reports | storage growth control for registries; cleanup policies alone do not cover tag-level retention | `RET-05`, `RET-06`, `RET-10`, `RET-03`, `RET-07` to `RET-09` | M | W2-C |
| 8 | Quotas: project hard quota and blob store soft quota | multi-team use needs a limit per team; compat and OCI peer each cover one half | `RET-11` to `RET-13`, `BLOB-12` | M | W2-C |
| 9 | Webhooks with HMAC signature, outbox and delivery log | integration with CI and chat; ТЗ phase 2 item | `OPS-01` to `OPS-05` | M | W2-E |
| 10 | Task administration: list, schedule, run now, stop, run history, notifications; GC history and dry-run reports | operators cannot manage or trust background jobs without it; the UI needs it for GC and lifecycle pages | `RET-14`, `RET-17` to `RET-19`, `OPS-07` | M | W2-C |
| 11 | Registry endpoints and replication (OCI first) | OCI peer's second pillar and the base for multi-site setups | `PRX-14` to `PRX-21` | L | W2-E |
| 12 | Copy, retag and promotion; labels and tags | release flow between repositories and projects; staging is a ТЗ phase 2 item | `UPL-09` to `UPL-14` | M | W2-E |
| 13 | Content trust: signature verification policy and accessory handling | Cosign and Notation are the supply chain baseline for images; needs a library spike | `SCN-22` to `SCN-25` | M to L | W2-D |
| 14 | SBOM generation, storage, display | follows the scanner; OCI peer ships it | `SCN-27` to `SCN-29` | S to M | W2-D |
| 15 | Blob store groups, Azure, S3 credential and encryption options, presigned redirects | compat storage parity and ТЗ phase 2 item | `BLOB-04` to `BLOB-07`, `BLOB-10`, `BLOB-11`, `BLOB-16`, `BLOB-19` | M | W2-F |
| 16 | System settings bundle: SMTP email, outbound HTTP proxy, managed trust store, read-only mode, runtime log levels, status checks | operations parity; read-only mode is also needed for consistent backups | `OPS-06`, `OPS-14`, `OPS-19`, `OPS-22`, `PRX-07`, `AUTH-85` | M | W2-H |
| 17 | More formats in ТЗ phase 2 order | compat offers 27 formats; Rustryx covers 7 | `REPO-20` to `REPO-31`, `REPO-42`, `REPO-46` | L (S to M per format) | W2-G |
| 18 | Identity extras: LDAP multi-server and verify tools, OIDC admin screen and logout, remote user token, default role, self-service profile | closes the Community identity gaps and removes file-only OIDC and LDAP setup | `AUTH-04` to `AUTH-06`, `AUTH-18`, `AUTH-19`, `AUTH-22`, `AUTH-24`, `AUTH-31`, `AUTH-46` | M | W2-H |
| 19 | Backup and restore verification, repository export and import, compat migration | adoption path from an existing compat and operational safety; ТЗ phase 2 items | `RET-22`, `RET-23`, 01-architecture phase 2 migration | L | W2-H |
| 20 | `ryx` CLI, client-setup snippets, first-run wizard with optional default repositories | day-one usability and parity with a typical registry CLI; small but visible | `OPS-39`, `SRCH-18`, `UPL-16`, `REPO-50` | M | W2-H |

## 14. Proposed split: wave 2 backend and UI milestones

### 14.1 Wave 2 backend streams

Streams are ordered by dependency. A, B and C unblock most UI work; D to H can run in parallel once the data model decisions in sections 11 and 12 are agreed. Proposed new crates: `rustryx-scan`, `rustryx-replication`, `rustryx-notify`, `rustryx-cli`; everything else extends existing crates.

| Stream | Content | Capability IDs | Depends on | New crates and modules |
|---|---|---|---|---|
| W2-A API and UI enablers | `/api/v1/` for all phase 1 entities, OpenAPI, browse tree, search and component detail, upload form schema and `POST /api/v1/components`, client setup, audit query, system status and info, `/me` | `SRCH-08` to `SRCH-14`, `SRCH-18`, `UPL-01` to `UPL-03`, `OPS-10`, `OPS-19`, `OPS-20`, `OPS-37`, `AUTH-31`, `AUTH-53`, `AUTH-54` | phase 1 | `rustryx-api` native router, `rustryx-db` browse nodes |
| W2-B Projects and tokens | projects, members, roles, group mapping to project roles, robot tokens, labels, token and project settings | `AUTH-42`, `AUTH-55`, `AUTH-56`, `AUTH-62` to `AUTH-67`, `AUTH-82`, `UPL-10`, `OPS-24` | W2-A | `rustryx-rbac` project role expansion, `rustryx-auth` token kinds |
| W2-C Lifecycle and capacity | download counters, retention, immutability, quotas, GC history and dry-run reports, task administration, read-only mode, label cleanup | `BLOB-12`, `BLOB-20`, `RET-05` to `RET-13`, `RET-17` to `RET-19`, `OPS-22`, `UPL-11` | W2-A, W2-B for project scope | `rustryx-tasks` lifecycle engine |
| W2-D Supply chain | scanner trait, Trivy and OSV adapters, findings, allowlists, pull gate, Security Hub, SBOM, trust policies, health summaries, offline data | `SCN-01` to `SCN-29` | W2-A, W2-B | `rustryx-scan` |
| W2-E Distribution | registry endpoints, replication, webhooks and outbox, email notifications, promotion and copy | `PRX-14` to `PRX-21`, `OPS-01` to `OPS-07`, `UPL-12`, `UPL-13` | W2-A, W2-C for tasks | `rustryx-replication`, `rustryx-notify` |
| W2-F Storage | blob store groups, Azure, S3 credentials and encryption, presigned redirects, `verify-blobs` | `BLOB-04` to `BLOB-07`, `BLOB-10`, `BLOB-11`, `BLOB-16`, `BLOB-19` | phase 1 blob traits | `rustryx-blob` |
| W2-G Formats | cargo, go, nuget, apt, yum first (they were in the 01-architecture phase 1 table); then conan, conda, terraform, rubygems, cran, alpine, huggingface; Docker subdomain; name claiming for npm, PyPI and Maven | `REPO-20` to `REPO-31`, `REPO-42`, `REPO-46` | `rustryx-format-core` contract | one crate per format |
| W2-H Platform and identity | settings bundle, LDAP multi-server and verify, OIDC admin screen and RP logout, remote user token, default role, CLI, backup, export and import, compat migration | `AUTH-04` to `AUTH-06`, `AUTH-18`, `AUTH-19`, `AUTH-22`, `AUTH-24`, `AUTH-46`, `AUTH-85`, `OPS-06`, `OPS-14`, `OPS-39`, `PRX-07`, `RET-22`, `RET-23` | W2-A | `rustryx-cli`, `rustryx-config`, `rustryx-auth` |

Items marked `later` stay outside wave 2: SAML (`AUTH-20`), IP allow list (`AUTH-28`), blob store change task (`BLOB-18`), Google Cloud Storage (`BLOB-08`), Ansible, CocoaPods, Composer, Git LFS, p2 and Swift formats, non-OCI content replication (`PRX-22`), license risk and malware features (`SCN-15` license part, `SCN-17`), support bundle (`OPS-21`), P2P preheat (`UPL-15`), job queue controls (`RET-20`), recovery mode (`RET-24`), banner and branding (`OPS-23`).

### 14.2 UI milestones

The page list and the per-page API and privilege tables are in [ui-page-inventory.md](ui-page-inventory.md).

| Milestone | Scope | Backend precondition |
|---|---|---|
| UI-M1 Usable core | login (local and OIDC), shell and navigation, dashboard, repositories (list, detail, create, edit), browse tree, component and asset detail, search, upload, `client setup` panel, own profile and tokens, administration of users, roles, privileges, content selectors, group mappings, anonymous access, blob stores, cleanup policies, routing rules, tasks (read only), audit viewer, system status | phase 1 plus W2-A (and routing rules from phase 1) |
| UI-M2 registry parity | projects with members, labels, policies and logs; robot tokens; OCI artifact detail with tags, vulnerabilities, SBOM and accessories; security center and scanners; allowlists; retention, immutability and quotas; GC with dry run; task administration; webhooks; registries and replication; promotion; trust policies; settings pages | W2-B to W2-E |
| UI-M3 Long tail and polish | usage and trend dashboards, jobs queue view, API explorer page, migration and backup pages, node status, banner and branding, IP allow list, command palette, saved views, accessibility and localization audit | W2-F to W2-H and the `later` items |

## 15. Sources

All pages were fetched on 2026-10-24 from public product documentation. Vendor names and vendor URLs are omitted on purpose (README clean-room rule). Capability notes still use internal keys `N-*` (compat Repository pages) and `H-*` (OCI-registry pages). Spec files are in `docs/TZ/nexspence-rust/` of the project docs.

### Rustryx specification

README.md sections 3, 5 and 8, `01-architecture.md` sections 1.1 to 1.6, `04-sre.md`, `06-devsecops.md` (DSO-34) and `07-implementation-prompt.md` in `docs/TZ/nexspence-rust/`.

## 16. Not verified, and open questions

- OCI peer email settings: the OCI peer pages read do not cover them, a web search found nothing from registry-docs.example. `OPS-06` treats it as `unverified` for OCI peer.
- OCI peer behaviour on group-to-project-role mapping for LDAP and OIDC groups: the LDAP page covers only the admin group; the OIDC page covers the admin group and a group filter.
- OCI peer vulnerability table columns, the pluggable scanner adapter contract, and how OCI peer verifies Cosign and Notation signatures: not documented on the pages read.
- OCI peer: interaction of tag retention with immutability, quota behaviour for proxy cache projects, whether re-pushing the same digest to an immutable tag is allowed.
- OCI peer offline vulnerability data import: a page title exists, the content was not read.
- OCI peer classic Helm chart repository (ChartMuseum): status not found; only OCI charts were confirmed.
- compat: edition for items that are neither on the matrix's Community list nor on its Pro-only lists is not confirmed (marked `npl`). Cleanup policies, tasks, audit, capabilities, webhooks edition detail, and the Docker V1 API fall in this group.
- compat: the Community requests-per-day limit is stated differently on different pages. Only the 40,000 component limit was confirmed on the tiers page.
- compat: default cleanup schedule differs between the cleanup policies page (hourly) and the tasks page (daily at 1 am).
- compat: run and stop controls for tasks, behaviour of tasks across HA nodes, REST status endpoint details, and the content of the user token and NuGet API key pages in the profile area.
- compat: the exact URL of the outbound trust store page and the support request page; snippets only.
- compat: Terraform group repository support is contradictory on the formats page.
- compat: whether routing rules apply to content that is already cached; whether proxy repositories show only cached content in the browse tree.
- compat Gradle format: listed in the feature matrix only; no format page was found.
- compat telemetry, EULA, license, Crowd and Atlassian pages: titles only; content not read.
- Verification libraries for Cosign and Notation in Rust: not verified; a spike is required (`SCN-25`).
- compat webhooks: the matrix lists them in the supply chain group as an add-on for Pro (Firewall license) and included in Pro+; `OPS-01` follows that statement, although the webhook capability pages themselves name no edition.
