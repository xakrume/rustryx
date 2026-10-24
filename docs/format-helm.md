# Classic Helm chart repositories

`rustryx-format-helm` serves `RepoFormat::Helm`: a classic Helm chart repository (`index.yaml` plus `charts/*.tgz`). This is not the OCI registry. OCI chart push is `rustryx-format-oci`.

`HelmHandler` implements `rustryx_format_core::FormatHandler`. Hosted, proxy, and group repositories are all served by the format-core pipelines. Register one instance for `RepoFormat::Helm`. The tests below are `cargo test -p rustryx-format-helm --all-targets`. No Helm binary is used.

## Paths

`classify` normalizes the path once. A path that contains `..` in any of the encodings the normalizer rejects (`charts/../index.yaml`, `charts/%2e%2e/secret.tgz`, `..%2findex.yaml`) is HTTP 400 and the body contains `invalid path`. That happens before the method is considered.

On a hosted repository the only addresses are:

| Path | Meaning |
|---|---|
| `index.yaml` | The generated index. `GET` and `HEAD` read it. `PUT` is 400. `DELETE` is 405 with `Allow: GET, HEAD`. |
| `charts/<name>-<version>.tgz` | One chart archive. `<name>` is a DNS label (lowercase letters, digits, and dashes, at most 63 characters, starting and ending with a letter or digit). `<version>` is a canonical semantic version (`1.2.3`, not `1.2` or `v1.2.3`). |

`PUT`, `GET`, or any other method on `charts/Web-1.0.0.tgz`, `charts/web-1.2.tgz`, or `charts/upload.tgz` is 400 and the body contains `DNS label`. A path that is not `index.yaml` and not under `charts/` is 400 and the body contains `not a helm chart repository path`. `POST` on a chart path is 405 with `Allow: GET, HEAD, PUT, DELETE` (handler unit test).

A proxy or a group classifies `charts/Web-1.0.0.tgz` as a chart file that can be fetched (unit test). A hosted repository refuses that name. An engine test of fetching a non-canonical name from an upstream is not in this crate.

`index.yaml` is `ContentKind::Index`. A chart path is an artifact. A strict chart path carries the coordinate `name` and `version` used by authorization.

## Hosted

### Upload

`PUT charts/<name>-<version>.tgz` reads the body, then runs `read_chart` (the archive reader in `chart`) on the blocking pool. Nothing is stored unless that reader accepts the archive.

The default reader caps are the `ChartLimits::default` values asserted by `default_limits_are_the_documented_caps`: 64 MiB compressed, 128 MiB decompressed, compression ratio 100, 10 000 entries, 1 MiB `Chart.yaml`. `HelmHandler::with_chart_limits` replaces them. A body whose `Content-Length` is above `max_archive_bytes` is 413 and the index stays empty (tested with a 32 byte cap).

Refusals that the engine tests pin down:

* A tar entry whose path leaves the archive (`web/../../etc/passwd`) is 400 and the body contains `unsafe path`. The index stays empty.
* An archive whose decompressed size exceeds the configured cap is 400 and the body contains `allowed size`. The index stays empty. The cap in that test is 16 KiB decompressed, with a 64 KiB payload of zeros.
* `Chart.yaml` whose `name` is not a DNS label (`Web`) is 400 and the body contains `lowercase`. Nothing is listed.
* The file name must be exactly `<name>-<version>.tgz` from `Chart.yaml`. `PUT charts/web-1.0.0.tgz` of a chart whose version is `2.0.0` is 400 and the body contains `does not match`. Neither `charts/web-1.0.0.tgz` nor `charts/web-2.0.0.tgz` is stored (`GET` of the canonical name is 404).

A chart that passes is stored at the same path the client sent (that path is what authorization saw). The first upload is 201. The response has no body. `Content-Type` of a later `GET` is `application/gzip`. The bytes are the uploaded archive. `HEAD` is 200, `Content-Length` is the archive size, and the body is empty.

The repository write policy applies. With `allow_once`, a second `PUT` of the same path is 409 and `GET` still returns the first archive.

`PUT index.yaml` is 400 even for a caller who may write. The body contains `generated`. A following `GET index.yaml` does not contain the uploaded document. The index is built from stored charts only.

### Index

`GET index.yaml` and `HEAD index.yaml` build the document from every stored `charts/<name>-<version>.tgz`:

* `apiVersion` is `v1`.
* `entries` is keyed by chart name. Versions of one chart are newest first (`1.2.0` before `1.0.0`).
* Each entry's `digest` is the lowercase hex SHA-256 of the stored archive.
* Each entry's `urls` has one absolute URL: `{base}/repository/{repository}/charts/{name}-{version}.tgz`. In the tests `base` is `https://rustryx.test`, so a repository named `charts` publishes `https://rustryx.test/repository/charts/charts/web-1.2.3.tgz`.
* `description` comes from `Chart.yaml` (`Test chart web` for the test helper).
* `created` and `generated` are present (RFC 3339 timestamps from the clock).

`Content-Type` is `application/yaml`. `HEAD` has an empty body and the same `Content-Length` as `GET`.

`DELETE charts/<name>-<version>.tgz` is 204. The file is then 404 and that version disappears from the index. Another version of the same chart stays.

An index lists at most `HelmHandler::with_max_index_charts` charts (the default constant is 4096; a value below 1 is raised to 1). One chart past the cap makes `GET index.yaml` return 422 and the body contains `more charts`. The archives themselves still download. The test uses a cap of 1 and two charts. A partial index is not returned.

### Authorization

The engine authorizes the classified action. With the real `FormatEngine` and an in-memory authorizer:

* An anonymous `GET` of `index.yaml` or of a chart, and an anonymous `PUT`, are 401.
* A caller granted read but not write can `GET` the index (200) and cannot `PUT` or `DELETE` a chart (403).
* A caller with no grant on the repository gets 404 for `GET index.yaml`, so the repository is not revealed.

## Proxy

`GET` and `HEAD` only. `PUT` is 405 with `Allow: GET, HEAD` (the engine refuses writes on a proxy before the handler runs).

`GET index.yaml` loads the upstream document through the proxy metadata cache (`X-Rustryx-Cache: MISS`, then `HIT`, one upstream call to `/index.yaml`). The handler parses it with the bounded YAML reader and rewrites every `urls` entry to `{base}/repository/{proxy}/charts/{file}`. The file name is the last safe `.tgz` segment of the upstream URL (query string ignored). The test upstream lists `https://upstream.example.test/charts/web-9.9.9.tgz` and the client receives `https://rustryx.test/repository/mirror/charts/web-9.9.9.tgz`. The upstream host is not left in `urls`. `Content-Type` is `application/yaml`.

A unit test covers the other half of that rewrite: an entry whose name is not a DNS label, whose version is not canonical, and whose URL does not end in a safe `.tgz` name loses its `urls` instead of keeping an upstream absolute URL. Versions are ordered newest first.

`GET charts/<file>.tgz` is a normal cached artifact fetch of the same path on the upstream. The test serves raw bytes `chart-bytes` (the proxy does not run the chart reader). The second `GET` is `X-Rustryx-Cache: HIT` and the upstream is called once.

## Group

`GET` and `HEAD` only. `PUT` of `index.yaml` on the group is 405. An anonymous `GET` of the group index is 401.

The engine test orders a hosted member before a proxy member. `GET index.yaml` on the group asks both:

* The hosted index contains `web` `1.0.0`. Its `urls` point at the group (`/repository/all/charts/web-1.0.0.tgz`), not at the hosted member. Members build URLs from the repository the client addressed.
* The proxy index contains `web` `9.9.9` and `other` `1.0.0`, rewritten to the group the same way.
* The merge keeps `web` `1.0.0` and drops `web` `9.9.9`, because a hosted member earlier in the group owns that chart name. `other` `1.0.0` from the proxy is kept. URLs in the merged document still point at the group.

`GET charts/web-1.0.0.tgz` on the group returns the hosted archive. `GET charts/web-9.9.9.tgz` is 404 and does not call the upstream (the chart name `web` is claimed because the hosted member has a component of that name). `GET charts/other-1.0.0.tgz` returns the proxy bytes.

A unit test of the merger, separate from the engine, checks that the first member's copy of one version wins: a later hosted member's `web` `1.0.0` with a description does not replace the first member's entry.

Deleting the hosted chart file (`DELETE` is 204, and that repository's index no longer lists it) does not drop the claim. The component row remains, so a later `GET charts/web-9.9.9.tgz` on the group is still 404 and the upstream is not called.

## What this crate does not do

* It does not shell out to Helm.
* It does not accept a client-supplied `index.yaml` as the index.
* Provenance files (`.tgz.prov`) are not part of the tested protocol surface.
* Fetching a non-canonical chart file name from a proxy is classified and not covered by an engine test.
