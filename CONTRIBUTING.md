# Contributing

## Build and checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Toolchain is pinned in `rust-toolchain.toml`. `Cargo.lock` is committed. New
dependencies go into `[workspace.dependencies]` of the root `Cargo.toml` with
the feature set fixed there, and crates use `dep.workspace = true`. Licenses
must be MIT compatible (MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode, MPL-2.0);
GPL, LGPL and AGPL crates are not allowed. No `openssl-sys`: TLS is rustls.

## Hard rules

1. `tokio` multi-thread runtime. No `std::fs`, `std::thread::sleep` or `block_on`
   in async code. No mutex guard (std, parking_lot, tokio, DashMap) is held across
   `.await`.
2. Artifact bodies are streamed, never collected into a `Vec<u8>`. Checksums
   (SHA-1, SHA-256, MD5) are computed incrementally in the same task over slices of
   at most 64 KiB and are not moved to `spawn_blocking`. `spawn_blocking` is for
   argon2id, bcrypt, archive unpacking and parsing of large XML or indexes, behind a
   semaphore whose permit is taken before spawning. No permit before the deadline
   means 503 with `Retry-After`.
3. `unsafe` is forbidden. `unwrap`, `expect`, `panic!`, `todo!` are denied outside
   tests (lints in the workspace manifest). Any `#[allow(...)]` carries a comment with the
   reason.
4. Deny by default: an endpoint without an explicit privilege requirement is
   unreachable. Anonymous access needs both the global switch and the repository flag.
5. Secrets come from environment variables or mounted files, never from the repo.
   They are redacted in logs, `Debug`, API responses and error messages.
6. `/healthz` touches no dependency. `/readyz` depends only on PostgreSQL (a
   comment in code states why the cascade risk is accepted).
7. Schema changes only through `rustryx migrate`; `serve` never migrates.
8. Every negative requirement ("never", "must not") has a test whose name starts
   with `must_not_`.
9. No em dash or en dash anywhere: code, comments, docs, commit messages.
10. Comments explain why, naming the mechanism that forces the choice. State the
    invariant and what holds it. Record the trap next to the code that would step
    into it.

## Clean room

Only public contracts are used: compat v1 REST API, protocol specifications of the
package formats, OIDC, LDAP. Do not copy code, database schema, documentation or UI
from AGPL-licensed artifact managers.

## Commits

`<type>: <description>` with types feat, fix, refactor, docs, test, chore, perf, ci.
