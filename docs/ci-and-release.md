# CI and release

How the delivery pipeline works, which gates block and which only report, how a
gate is switched to blocking, and how a release is made, signed and verified.
GitHub Actions is the primary CI (`.github/workflows/`). The same stages and
scripts also run on the self-managed GitLab at
<https://gitlab.xakru.me/rustryx/rustryx> (group `rustryx`, project `rustryx`,
file `.gitlab-ci.yml`). Supply-chain policy
(licences, advisories, SLA, exceptions) is in [supply-chain.md](supply-chain.md),
the developer environment in [local-development.md](local-development.md).

## Files

| File | Purpose |
|---|---|
| `.github/workflows/ci.yml` | lint, ui, test, code-security, build, image-security, publish, sign, provenance, verify |
| `.github/workflows/nightly.yml` | nightly `cargo audit`, expiry of exceptions, `cargo fuzz` (10 minutes per target) |
| `.github/dependabot.yml` | keeps the SHA pins of actions and base images current, with a 14 day cooldown |
| `.gitlab-ci.yml` | the same pipeline on `gitlab.xakru.me`, group `rustryx` |
| `Dockerfile`, `Dockerfile.musl` | glibc image (distroless/cc) and static image (distroless/static) |
| `deny.toml`, `.cargo/audit.toml` | `cargo deny` and `cargo audit` configuration |
| `ci/*` | the scripts both pipelines run (table below) |
| `deploy/compose/` | local stack and the shared Keycloak and OpenLDAP fixtures |

## Pipeline

```
meta
 |-- lint --------------+
 |-- ui                 |
 |-- architecture       |
 |-- test (+coverage)   |
 |-- fixtures           |-- build (glibc and musl, amd64 and arm64: 4 jobs)
 |-- code-security      |      pushes untagged digests (main and tags only)
 '-- semgrep (optional) |
                        v
              image-security (Trivy per digest)  --- blocks on CRITICAL
                        v
                     publish   creates the tags, only here
                        v
        sign (cosign)  and  provenance (SLSA)
                        v
                      verify   cosign verify, verify-attestation, slsa-verifier
```

Pull requests run `lint`, `ui`, `architecture`, `test`, `fixtures`, `code-security`
and `build` (the image is built, loaded and tested, nothing is pushed). Pushes to
`main` and tags `vX.Y.Z` run the whole chain. A run of `workflow_dispatch` on
another branch builds but does not publish.

A new push to a pull request cancels the older run (`concurrency` with
`cancel-in-progress` for `pull_request` events only). Runs of `main` and of tags
are never cancelled: cancelling one could leave a tag without an image.

## Jobs and their mode

| Job | What it checks | Mode |
|---|---|---|
| `lint` | `cargo fmt`, `clippy -D warnings --locked`, hadolint, yamllint, actionlint, shellcheck, the Python tests of `ci/`, compiler and base image pins, `ci/release-check.sh` on tags | blocking |
| `ui` | `npm ci` in `ui/`, then `npm run check` (typecheck, lint, vitest, licences) and `npm run build`; same as `make ui-check` / `make ui-build` | blocking |
| `architecture` | `easement check` (full on `main`, `--diff origin/main` on pull requests) | blocking |
| `test` | unit tests and in-process integration tests (nextest under llvm-cov), doc tests, JUnit, Cobertura. Docker/testcontainers tests are excluded | blocking |
| `test`, coverage step | 80% workspace, 90% for `rustryx-auth`, `rustryx-rbac` and the format parser crates, no drop against main | observation |
| `fixtures` | `ci/verify-fixtures.sh`: realm and LDIF behave as README 8.6 needs | blocking |
| `code-security`, every step | exceptions record, `cargo deny`, deny.toml negative tests, `cargo audit` (high and above), gitleaks, Trivy on `Cargo.lock`, SBOM not empty | observation |
| `semgrep` | `p/rust`, only when the repository variable `ENABLE_SEMGREP` is `true` | observation |
| `build` | the four images, `ci/test-image.sh` on each | blocking |
| `image-security` | Trivy per digest, component count, SBOM with Rust crates | CRITICAL and a broken scanner block, the rest follows `SECURITY_GATES_ENFORCE` |
| `publish` | tags, immutability guard | blocking |
| `sign`, `provenance`, `verify` | signature, SBOM attestation, SLSA provenance, verification by digest | blocking |
| `nightly` | `cargo audit`, `cargo deny check advisories`, exceptions not expired, fuzzing | blocks nothing, a red run is the alert |

GitHub's `test` job does not start Docker. The nextest filter drops the
testcontainers binaries: `rustryx-db` `it` (PostgreSQL), `rustryx-server`
`postgres_serve` and `postgres_tasks`, `rustryx-blob` `s3_seaweed`, `rustryx-auth`
`oidc_keycloak`, and `ldap::integration_tests::openldap_with_the_shared_fixture`.
`RUSTRYX_IT` is unset, so any other real-client test skips. Run those locally
with Docker (`cargo test -p rustryx-db`, and `RUSTRYX_IT=1` for SeaweedFS, Keycloak,
OpenLDAP and real clients). GitLab's `test` job still runs the full set.
`nightly.yml` does not run them.

There is no `deploy` job. Deployment is by GitOps or `helm upgrade` and belongs
with the chart in `deploy/helm/`; a job that does not deploy would be an empty job.

## Observation mode and how to end it

A new gate first reports (DSO-17). In a GitHub workflow every such step carries a
line `continue-on-error: true # OBSERVATION MODE ...`; in `.gitlab-ci.yml` it is
`allow_failure: true # OBSERVATION MODE ...`. List them:

```
grep -rn "OBSERVATION MODE" .github .gitlab-ci.yml
```

The scanner's own exit code and findings are written to the job summary, so a
broken tool does not look like a finding. Two traps that the design avoids:

* A job with `continue-on-error: true` reports **success** to its dependants
  (`needs.<job>.result`). That is why `image-security`, which `publish` waits for,
  has no such flag: it would let a CRITICAL image through. The observation flag
  is only on steps of jobs that nothing publishes behind.
* A green security job proves nothing. Every scanner step also checks that it
  looked at something: `cargo audit` and `ci/audit-gate.py` need crates and
  advisories, Trivy needs a package count, the SBOM needs components with a
  `purl`, gitleaks needs scanned bytes. Zero means exit code 3, never green.

To make one gate blocking:

1. Read its report from the first run (job summary and artifacts).
2. Fix the findings, or accept them with a recorded exception
   ([supply-chain.md](supply-chain.md), section Exceptions).
3. After one clean run, open a pull request of its own that deletes the one
   `continue-on-error` or `allow_failure` line of that gate. One gate per pull
   request, so the history records the date of every transition.

`SECURITY_GATES_ENFORCE` (top of `ci.yml`, a CI/CD variable of `.gitlab-ci.yml`)
is the second switch. It only concerns findings of the Trivy scans:

| Setting | CRITICAL (fixable) | HIGH | MEDIUM and LOW |
|---|---|---|---|
| `"false"` (default) | blocks | warning | warning |
| `"true"` | blocks | blocks | warning |

Findings without a fixed version are counted and printed but never block;
`ci/check-trivy.py --include-unfixed` changes that.

## The scripts in `ci/`

All have unit tests in `ci/tests/` (`python3 -I -m unittest discover -s ci/tests`),
including a test for each "must not".

| Script | Does |
|---|---|
| `audit-gate.py` | severity gate over `cargo audit --json` (CVSS 3.x scored, unscored blocks) |
| `check-exceptions.py` | exceptions have issue, reason, unexpired date; both ignore lists equal; licence allowlist exact |
| `check-trivy.py` | judges a Trivy JSON report: exit 1 block, 2 warning, 3 scanner did nothing |
| `check-sbom.py` | fails on an SBOM without components |
| `coverage-gate.py` | thresholds and regression against main |
| `check-gitlab-ci.py` | every stage used is declared, `allow_failure` only with the marker |
| `check-toolchain-pin.sh` | compiler version and every `FROM` digest agree across files |
| `release-check.sh` | tag, workspace version, `Chart.yaml` version and `appVersion` are equal |
| `install-tool.sh`, `tools.lock` | installs hadolint, actionlint, gitleaks, trivy, cosign after a SHA-256 check |
| `test-image.sh`, `check-image-labels.sh` | image acceptance: user, labels, no shell, read-only root, linkage |
| `verify-fixtures.sh`, `keycloak_checks.py` | boots the compose stack and checks the shared fixtures |
| `tests/deny-negative.sh` | `deny.toml` rejects a GPL crate, a git dependency and `openssl-sys` |
| `licence-hash.py` | the hash `licenses.clarify` needs |
| `gitlab-provenance.sh` | SLSA predicate for the GitLab option |

## Repository settings

No secret is needed: registry access uses the job token. These are variables
(Settings, Secrets and variables, Variables), all optional unless a mode needs them:

| Variable | Meaning |
|---|---|
| `RUSTRYX_SIGNING` | `keyless` (default), `kms` or `both` |
| `COSIGN_KMS_KEY_REF` | key reference for `kms` and `both`, for example `hashivault://rustryx-release` |
| `VAULT_ADDR`, `VAULT_JWT_ROLE`, `VAULT_JWT_AUDIENCE` | Vault or OpenBao login of the workflow (audience defaults to `rustryx-release`) |
| `IMAGE_SIZE_BUDGET_MB` | fails `ci/test-image.sh` above this uncompressed size; unset means report only |
| `RUSTRYX_SEAWEEDFS_IMAGE` | SeaweedFS image for the S3 integration test (default `chrislusf/seaweedfs:4.13`) |
| `ENABLE_SEMGREP` | `true` turns the Semgrep job on |

Also set: required status checks `lint`, `ui`, `architecture`, `test`,
`fixtures`, `code-security`, `build` on `main`; protected tags `v*` created by
the release role only; the package `ghcr.io/<owner>/rustryx` public after the
first publish (a new GHCR package is private).

Branch protection / rulesets need GitHub Pro on a private repository (or a public
repo). Until that is available, the `ci` workflow still runs and reports checks on
every pull request and every push to `main`; they are not merge-blocking yet.

`RUSTRYX_SEAWEEDFS_IMAGE`: the S3 integration test and the local compose stack
use `chrislusf/seaweedfs:4.13`. Set the variable when that tag cannot be pulled.

## Images and tags

Two images, never mixed under one tag (README 7). Repository
`ghcr.io/<owner>/rustryx`:

| Tag | Content |
|---|---|
| `X.Y.Z-musl`, `X.Y.Z-glibc` | release, one multi-arch manifest each (amd64, arm64) |
| `sha-<7 hex>-musl`, `sha-<7 hex>-glibc` | every publishing run of `main` and of tags |

The Helm chart picks the variant with `image.variant` and appends the suffix.
Rules enforced by the `publish` job, because GHCR has no tag immutability:

* `latest` and every other tag shape are refused (a regular expression guard).
* A tag that exists must point at exactly the two images of the current run;
  anything else fails the job. Re-running only `publish` is therefore safe.
* Nothing is tagged before `image-security` passed; `build` pushes untagged digests.

Pin deployments by digest (`image.digest`), not by tag. If the registry supports
immutable tags, switch them on for the pattern `*.*.*-*`.

Build facts: the builder is `rust:<toolchain>` pinned by digest, the final image is
`distroless/cc` or `distroless/static` pinned by digest, UID/GID 10001, no shell,
no `HEALTHCHECK`, writable only `/tmp` and `/var/lib/rustryx`. The binary is built
with `cargo auditable build --locked --release`: the dependency list is embedded,
which is the only reason Trivy can see the Rust crates in an image (without it a
scan lists the six Debian packages and calls the image clean). The musl image is
checked static at build time and by `ci/test-image.sh`. Debug symbols are exported
as a separate artifact (`debug-symbols-<variant>-<arch>`, 90 days).

Choosing a variant: glibc is the conservative default. The musl allocator is
slower under heavy multi-threaded allocation; measure before choosing musl for a
busy node.

## Release procedure

1. In one commit set the new version in `Cargo.toml` (`[workspace.package]`),
   refresh `Cargo.lock` so that it carries the workspace crates at that version
   (`cargo update --workspace`), and set `version` and `appVersion` of
   `deploy/helm/rustryx/Chart.yaml`. The tag is the one source (OPS-70):
   `ci/release-check.sh vX.Y.Z` fails the pipeline when the others differ.
2. Merge, then create a signed tag on the merge commit as the release role:
   `git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`.
3. Choose the signing method for this release: the repository variable
   `RUSTRYX_SIGNING`, or the `signing` input when the workflow is started by hand.
4. Wait for the run. `verify` is green only when the signature, the SBOM
   attestation and the SLSA provenance verified for both images.
5. Publish the GitHub Release with notes taken from the changelog: list schema
   migrations and incompatible changes (OPS-71). This step is manual; there is no
   changelog generator in the repository yet.
6. Deploy by digest. The digests are in the summary of the `publish` job.

Rollback is a revert of the manifest change, not a history rewrite; the database
schema stays compatible with N-1 (OPS-72).

### What a consumer verifies

```
image=ghcr.io/<owner>/rustryx
digest=<sha256 from the release notes or the publish summary>

cosign verify "$image@$digest" \
  --certificate-identity-regexp '^https://github.com/<owner>/rustryx/\.github/workflows/ci\.yml@refs/tags/v[0-9.]+$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
cosign verify-attestation "$image@$digest" --type cyclonedx \
  --certificate-identity-regexp '<same>' --certificate-oidc-issuer https://token.actions.githubusercontent.com
slsa-verifier verify-image "$image@$digest" --source-uri github.com/<owner>/rustryx --source-tag vX.Y.Z
```

For the key in KMS: `cosign verify --key ci/cosign.pub "$image@$digest"`.

## Signing

The publisher of a release chooses (README 7). Both paths sign the **index**
digest, the one a tag resolves to, and attach the SBOM as a `cyclonedx`
attestation.

* **keyless** (default): cosign exchanges the job's GitHub OIDC token for a
  short-lived Fulcio certificate and records the signature in Rekor. The identity
  is the workflow file and the ref, which is what `verify` checks. Needs a public
  repository (Rekor is public).
* **kms**: the key stays in Vault, OpenBao or a cloud KMS and cosign gets a
  reference (`hashivault://rustryx-release`, `awskms://...`). The workflow logs in
  to Vault or OpenBao with its own GitHub OIDC token (JWT auth method) and receives
  a short-lived token (`hashicorp/vault-action`, `exportToken`). **No private key
  material and no static token is stored in CI variables.** A gitleaks rule
  (`ci/gitleaks.toml`) fails on `secrets.COSIGN_PRIVATE_KEY` and on cosign key
  files, so this cannot slip in.
* **both**: both signatures on the same digest; `verify` checks both.

Set-up for OpenBao or Vault, once, by the secrets administrator:

```
bao secrets enable -path=transit transit
bao write -f transit/keys/rustryx-release type=ecdsa-p256
bao policy write rustryx-sign - <<'HCL'
path "transit/sign/rustryx-release"   { capabilities = ["update"] }
path "transit/sign/rustryx-release/*" { capabilities = ["update"] }
path "transit/keys/rustryx-release"   { capabilities = ["read"] }
HCL
bao auth enable jwt
bao write auth/jwt/config oidc_discovery_url=https://token.actions.githubusercontent.com \
  bound_issuer=https://token.actions.githubusercontent.com
bao write auth/jwt/role/rustryx-release role_type=jwt user_claim=repository \
  bound_audiences=rustryx-release token_policies=rustryx-sign token_ttl=10m \
  bound_claims_type=glob \
  bound_claims='{"repository":"<owner>/rustryx","job_workflow_ref":"<owner>/rustryx/.github/workflows/ci.yml@refs/*"}'
```

Then set `COSIGN_KMS_KEY_REF=hashivault://rustryx-release`, `VAULT_ADDR`,
`VAULT_JWT_ROLE=rustryx-release`, and commit the public key so `verify` can work
offline: `VAULT_ADDR=... cosign public-key --key hashivault://rustryx-release > ci/cosign.pub`.
Pushes to pull-request branches cannot sign: they do not publish.

## SBOM and provenance

* **Image SBOM**: Trivy writes CycloneDX per image (`image-security`), checked for
  Rust components. The amd64 SBOM of a variant is attached to its index; the arm64
  image has the same crates and Debian packages, only the architecture differs.
* **Source SBOM**: `cargo cyclonedx` for the server binary in `code-security`, an
  independent generator as a cross-check.
* **Provenance**: the official `slsa-github-generator` container workflow (SLSA
  Build L3 by construction, above the L2 target). It is a reusable workflow and
  must be called by tag, `@v2.1.0`: its builder identity is checked by that tag, a
  commit SHA is not accepted there. This is the only reference that is not a SHA.
  The GitLab option cannot use it; `ci/gitlab-provenance.sh` makes a predicate in
  a pipeline job and the pipeline signs it, which is a signed statement of the
  platform but not an isolated builder, so claim L2 at most there.

## Nightly and fuzzing

`nightly.yml` runs at 02:17 UTC. The audit job fails on high and above, prints
every unmaintained and unsound warning, and fails when an exception has expired.
Fuzz targets are the files of `fuzz/fuzz_targets/*.rs`: the matrix is built from
the directory, so a new target is fuzzed the next night without editing the
workflow, and without targets the job is skipped. Each target runs
`FUZZ_SECONDS` (600) on a pinned nightly toolchain, the corpus is cached between
nights, crashing inputs are uploaded for 90 days.

## Pinned actions

Every third-party action is pinned by commit SHA with the version in a comment.
Each SHA was resolved from its tag and checked back with the GitHub API. Versions
are releases older than two weeks at the time of writing.

| Action | Version |
|---|---|
| `actions/checkout` | v7.0.1 |
| `actions/setup-node` | v6.5.0 |
| `actions/upload-artifact` | v7.0.2 |
| `actions/download-artifact` | v8.0.2 |
| `actions/cache` (restore, save) | v6.1.0 |
| `Swatinem/rust-cache` | v2.9.2 |
| `taiki-e/install-action` | v2.87.27 (installs cargo-deny, cargo-audit, cargo-nextest, cargo-llvm-cov, cargo-cyclonedx, trivy, cosign with its own checksums) |
| `docker/setup-buildx-action` | v4.4.1 |
| `docker/login-action` | v4.6.0 |
| `docker/build-push-action` | v7.4.0 |
| `hashicorp/vault-action` | v4.0.0 |
| `slsa-framework/slsa-verifier/actions/installer` | v2.7.1 |
| `slsa-framework/slsa-github-generator` | v2.1.0, **by tag**, required by the generator |

Not installed by an action, pinned by SHA-256 in `ci/tools.lock`: hadolint,
actionlint, gitleaks (and trivy, cosign for GitLab). Python tools: hash-pinned in
`ci/requirements-lint.txt`. Container images (Semgrep, Docker, Alpine, Node 22 for
the GitLab `ui` job, builders, distroless) by digest. To update, take the
Dependabot pull request or repeat the resolution by hand and run
`ci/check-toolchain-pin.sh`.

## GitLab (`gitlab.xakru.me`)

The GitLab home of this tree is its own group:
<https://gitlab.xakru.me/rustryx/rustryx>. Creating that group and project needs
a personal access token with the `api` scope on `gitlab.xakru.me`. This
repository does not store that token. After the first push of the default
branch, merge requests and version tags start `.gitlab-ci.yml`.
The `workflow` rules already limit pipelines to merge requests, the default
branch, and version tags, so every check below is the project's CI. There is
no second, shorter pipeline.

| Check | Stage | What it gates |
|---|---|---|
| `lint` | lint | `cargo fmt`, Clippy, hadolint, yamllint, actionlint, shellcheck, OpenAPI, toolchain pins |
| `architecture` | lint | `easement check` |
| `ui` | lint | typecheck, lint, unit tests, production UI build |
| `test` | test | workspace tests, including Docker integration tests (`RUSTRYX_IT=1`) |
| `coverage-gate` | test | coverage report (observation until the marker is removed) |
| `fixtures` | test | compose fixtures on a shell runner (`FIXTURES_RUNNER_TAG`) |
| `cargo-audit` | code-security | Rust advisories |
| `cargo-deny` | code-security | licences and bans |
| `secret-detection` | code-security | gitleaks |
| `dependency-scan` | code-security | Trivy on the lockfiles |
| `sbom` | code-security | CycloneDX from the Rust build |
| `build-amd64`, `build-arm64` | build | glibc and musl images, untagged digests |
| `image-security` | image-security | Trivy on the built images |
| `publish` | publish | tags the images (default branch and `vX.Y.Z` only) |
| `sign` | sign | cosign |
| `provenance` | sign | SLSA-style predicate (`ci/gitlab-provenance.sh`) |
| `verify` | verify | signature and provenance |

Jobs marked `allow_failure: true # OBSERVATION MODE` report without failing the
pipeline. Removing that line makes the check blocking. `build-arm64` needs a
runner whose tag is `saas-linux-medium-arm64` (or the instance's own tag in
`RUNNER_ARM64`). `fixtures` needs the shell-executor tag in
`FIXTURES_RUNNER_TAG` (default `docker-shell`). The other jobs use a Docker
executor.

Same stages and scripts. Differences worth knowing:

* Every stage a job uses is declared in `stages`; `ci/check-gitlab-ci.py` checks
  it in `lint`, and the merged YAML can be checked on the server with
  `POST /projects/:id/ci/lint?include_merged_yaml=true`.
* `fixtures` needs a shell-executor runner (tag in `FIXTURES_RUNNER_TAG`), because
  the stack is queried on `localhost`; docker-in-docker publishes its ports on
  another host. `test` uses docker-in-docker with `TESTCONTAINERS_HOST_OVERRIDE`
  and `RUSTRYX_IT=1`, so it still runs the Docker integration tests that GitHub's
  `test` job skips.
* The arm64 jobs use the tag `saas-linux-medium-arm64`; change it on a
  self-managed instance.
* Keyless signing uses the job's `SIGSTORE_ID_TOKEN`; the verification identity is
  `<server>/<project>//.gitlab-ci.yml@refs/...`.
* GitLab validates the stage of a job before its rules, so a stage missing from
  `stages` breaks the whole pipeline even when the job would have been skipped.
* Some GitLab installations put bot protection in front of the API and answer
  HTTP 418 to frequent polling, which looks like an outage. Do not poll the API in
  a loop to wait for a pipeline; back off.

## Known limits

* No `deploy` stage (see above).
* Docker builds cannot share Cargo's `target/` through the GitHub cache: BuildKit
  cache mounts are not exported, so each image build compiles dependencies cold.
  Layer cache (`type=gha`) still serves unchanged layers. Measure the `build` job
  duration after the first runs and decide on thresholds (OPS-06).
* No HTML coverage or Grype second scanner; Trivy is the only image scanner.
* Chart tests (`helm lint`, kubeconform, unittest, a manifest scanner) are not in
  this pipeline; they belong with the chart.
* `ci/test-image.sh --with-serve` (migrate and serve on a read-only filesystem
  against a database) is implemented and not enabled in CI until `rustryx serve`
  exists. `crates/rustryx-server` must declare `[[bin]] name = "rustryx"`: the
  Dockerfiles fail with that message otherwise.
