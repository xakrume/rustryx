# Rustryx

A self-hosted artifact manager written in Rust. Hosted, proxy and group
repositories for maven2, npm, pypi, docker/oci, helm, raw and pub (Dart and
Flutter). Authentication through Keycloak (OIDC), LDAP and local accounts.
RBAC with roles assigned from Keycloak and LDAP groups. compat v1 REST API
compatible where CI clients need it. Streaming everywhere, stateless nodes,
PostgreSQL for metadata, local disk or S3-compatible storage for blobs.

License: MIT. The implementation is clean-room: it follows public protocol
specifications and the public compat v1 REST API contract, not the source,
schema or documentation of any other artifact manager.

Status: under construction (phase 1). See `CONTRIBUTING.md` for the workspace
layout and the rules every change follows, and `docs/` for design notes.

## Workspace

| Crate | Purpose |
|---|---|
| `rustryx-core` | domain types, ports (traits), errors |
| `rustryx-config` | configuration loading and validation |
| `rustryx-observability` | tracing, metrics, OTLP |
| `rustryx-db` | PostgreSQL: migrations and port implementations |
| `rustryx-blob` | local and S3 blob stores |
| `rustryx-auth` | local, API token, OIDC and LDAP authentication, group resolution |
| `rustryx-rbac` | privileges, roles, content selectors, authorization |
| `rustryx-format-core` | shared hosted, proxy, group pipeline |
| `rustryx-format-{maven,npm,pypi,oci,helm,raw,pub}` | one crate per package format |
| `rustryx-api` | axum router, middleware, compat v1 REST API |
| `rustryx-tasks` | scheduler and background jobs |
| `rustryx-server` | the `rustryx` binary (`serve`, `migrate`) |
| `rustryx-testkit` | in-memory ports and fixtures for tests |
