# Supply chain

What the project checks about the code and the images it ships, how fast findings
must be fixed, and how an exception is granted. Pipeline mechanics are in
[ci-and-release.md](ci-and-release.md).

## What is checked

| Check | Tool and config | Fails on |
|---|---|---|
| Advisories | `cargo deny` (`deny.toml`), `cargo audit` (`.cargo/audit.toml`) | any vulnerability or yanked crate; `cargo audit` also by CVSS 7.0 and above through `ci/audit-gate.py` |
| Licences | `cargo deny` | a licence outside the allowlist |
| Bans | `cargo deny` | `openssl`, `openssl-sys`, `native-tls`; wildcard version requirements on registry crates |
| Sources | `cargo deny` | any registry other than crates.io, any git dependency |
| Duplicates | `cargo deny` | warning only, with a documented skip list |
| Secrets | gitleaks (`ci/gitleaks.toml`) | a secret in history or tree, a cosign key, a workflow reading one |
| Dependencies | Trivy on `Cargo.lock` | fixable CRITICAL (always) and HIGH (when enforced) |
| Image | Trivy on every published digest | same, plus "scanned nothing" |
| SBOM | `cargo cyclonedx`, Trivy CycloneDX | zero components with a `purl` |
| Reproducible input | `--locked`, pinned toolchain and base images | a lockfile out of date, a floating tag |

The checks run in CI. To run them locally:

```
cargo deny check                         # all four deny checks
cargo audit --json > audit.json; python3 ci/audit-gate.py audit.json
python3 ci/check-exceptions.py           # exceptions recorded and not expired
ci/tests/deny-negative.sh                # deny.toml still rejects GPL, git, openssl-sys
python3 -I -m unittest discover -s ci/tests
```

Other contributors add dependencies concurrently, so rerun `cargo deny check` after
every `Cargo.lock` change; a new duplicate shows up as a warning that is not in the
skip list, a new licence or source as an error.

## Licences

Allowed, exactly (README 8.6): MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC,
Zlib, Unicode-3.0, Unicode-DFS-2016, MPL-2.0. `ci/check-exceptions.py` fails when
`deny.toml` drifts from this list, so widening it is a reviewed change to two files.

* A crate whose SPDX expression offers an allowed alternative passes through it
  (`MIT OR 0BSD` is fine). GPL, LGPL and AGPL are never in the list; a crate that
  offers only those is rejected.
* **MPL-2.0** is file-level copyleft and is acceptable only while the dependency is
  used **unmodified**. Patching or vendoring an MPL crate would oblige the project
  to publish those files under MPL-2.0: that needs a maintainer decision first.
  `[patch]` and git sources are blocked by the `sources` section anyway.
* **ring** and **aws-lc-sys** carry `licenses.clarify` entries pinned to the hash of
  their licence file (`ci/licence-hash.py` prints it). When a release changes the
  licence text, the hash stops matching, cargo-deny falls back to the manifest
  expression, and a human re-reads the licence.
* Per-crate exceptions are in `licenses.exceptions`, each with a comment
  `# exception <crate>: <reason>`. Today: `webpki-roots` (CDLA-Permissive-2.0, the
  permissive data licence of the Mozilla root list).

## Advisories

* Vulnerabilities and yanked crates fail (`deny.toml`), `cargo audit` fails at CVSS
  7.0 (high) and above and on any vulnerability it cannot score.
* **Unmaintained and unsound** are warnings for transitive crates: cargo-deny 0.20
  hides them (it fails only for a direct dependency of a Rustryx crate), `cargo
  audit` prints them, and the nightly run repeats them. They follow the SLA below.
* Known today:

| Item | Where it comes from | Handling |
|---|---|---|
| RUSTSEC-2023-0071 (`rsa`, Marvin timing attack) | `jsonwebtoken` and `openidconnect` | exception until 2027-01-22, see `deny.toml`: Rustryx only verifies RS256 signatures and never signs or decrypts with an RSA key; no patched release exists |
| RUSTSEC-2024-0449 (`unsafe-libyaml` unmaintained) | `serde_yaml_ng` of `rustryx-config` | warning; replace the YAML parser (safe-Rust options exist) within the MEDIUM window |

## Service level for findings

Counted from the day the finding is first reported by a pipeline or by the nightly run.

| Severity | Fix or record an exception within |
|---|---|
| CRITICAL | 7 days |
| HIGH | 30 days |
| MEDIUM | 90 days |
| LOW | the next minor release |

CRITICAL blocks `publish` regardless of `SECURITY_GATES_ENFORCE`. A fix is an
upgrade, a replacement of the crate or base image, or a patch of the code that
removes the exposure; an exception is not a fix.

## Exceptions

An exception is a reviewed, dated, justified decision recorded **next to the thing it
excuses**. An exception without all of the following is refused by CI.

| Kind | Where | Record |
|---|---|---|
| Advisory (RUSTSEC) | `advisories.ignore` in `deny.toml` **and** `ignore` in `.cargo/audit.toml` | `reason = "issue=<ref>; until=YYYY-MM-DD; <why it is acceptable, 20+ characters>"` |
| Licence | `licenses.exceptions` in `deny.toml` | the comment `# exception <crate>: <reason>` above the entry |
| Duplicate crate | `bans.skip` in `deny.toml` | a `reason` naming the dependency that forces it |
| CVE in an image or in `Cargo.lock` | `ci/trivyignore.yaml` | `id`, `statement` naming the issue, `expired_at` |
| A gate switched off or back to observation | the `OBSERVATION MODE` line in the workflow | the pull request that adds it states the reason; an expiry belongs in the issue |

Rules:

1. The issue is linked in the record, and a maintainer approves the pull request that
   adds the exception (DSO-15).
2. `until` is at most 180 days ahead. Renewal is a new issue and a new review, not an
   edit of the date alone.
3. An expired exception turns the pipeline red by itself: `ci/check-exceptions.py`
   checks the date for advisories, Trivy stops honouring an expired `expired_at`,
   and the nightly run checks again every night.
4. Both advisory lists must contain the same ids, otherwise one tool still fails.
5. An exception that is no longer needed is removed (`unused-ignored-advisory` and
   `unused-license-exception` are errors for that reason).

`cargo-deny` 0.20 has no native expiry for an ignore, so the date lives in the
`reason` and the script enforces it.

## Duplicate crates

`bans.multiple-versions = "warn"` with a skip list. Every entry names the lagging
version and the dependency that forces it: `openidconnect` and `oauth2` still use
reqwest 0.12, thiserror 1 and base64 0.21; `jsonwebtoken` with the `rust_crypto`
backend and `openidconnect` pull the 2023 RustCrypto generation (digest 0.10,
sha2 0.10, rsa 0.9) while `sqlx` and the workspace use the 2025 one; the syn 2 to
syn 3 move is in progress for proc macros. Remove an entry when the upstream moves,
cargo-deny warns about a skip that no longer matches. A **new** duplicate is a
warning that is not in the list: either upgrade the cause or add a justified entry.

## Sources

Only crates.io. Git dependencies bypass the lockfile checksum and the advisory
database (RustSec matches registry crates), so `unknown-git = "deny"` and
`allow-git = []`. A mirror of crates.io is allowed if its address is set in
`.cargo/config.toml` and added to `allow-registry`, with a maintainer decision.

## Images

* Every `FROM` is pinned by digest, the builder tag equals `rust-toolchain.toml`,
  `ci/check-toolchain-pin.sh` enforces both. Dependabot proposes updates; a builder
  bump needs `rust-toolchain.toml` changed in the same pull request, or the check is
  red on purpose.
* The final images contain only the binary, the CA bundle of distroless and the
  licence file. Trivy is never part of the image.
* The binary is built with `cargo auditable`, so the scan sees its crates. A scan that
  reports fewer than 10 packages, or an SBOM without `pkg:cargo/` components, is a
  failure (exit code 3), not a clean result.
* The scanner exit code is recorded: Trivy runs with `--exit-code 0` and its real
  status goes to `ci/check-trivy.py`, so a broken scanner cannot look like a clean image.

## Tools and their integrity

| Tool | Installed through | Integrity |
|---|---|---|
| cargo-deny, cargo-audit, cargo-nextest, cargo-llvm-cov, cargo-cyclonedx, trivy, cosign | `taiki-e/install-action`, version fixed in the workflow | action pinned by SHA, checksums in its manifest |
| hadolint, actionlint, gitleaks, trivy, cosign (GitLab) | `ci/install-tool.sh` | SHA-256 in `ci/tools.lock`, mismatch refuses |
| yamllint | `pip --require-hashes` from `ci/requirements-lint.txt` | every file hash listed, `--no-deps` |
| cargo-auditable, cargo-fuzz | `cargo install --locked --version` | crates.io checksum, locked |

## Signing keys

Private key material is never stored in CI variables, secrets, or git. Keyless
signing has no key; key-based signing keeps the key in Vault, OpenBao or a KMS and
the workflow holds only a reference and a short-lived token obtained with its own
OIDC identity. `ci/gitleaks.toml` fails on cosign key files and on a workflow that
reads `secrets.COSIGN_PRIVATE_KEY`. Details: [ci-and-release.md](ci-and-release.md).
