# Rustryx web UI: page inventory and information architecture

Status: design draft, 2026-10-24. Companion document: [parity-compat-harbor.md](parity-compat-harbor.md), whose capability IDs (for example `SRCH-10`) are cited here.

This document merges the screens a user of Sonatype compat Repository 3 and of Harbor 2 sees into one proposed information architecture for the Rustryx web UI. It lists every page with its purpose, main components, the native API resources it needs, the privilege it requires, the states to design, and the UI milestone it belongs to. Format-specific client configuration (mvn, npm, pip, docker, helm, pub) is one reusable `client setup` panel, not separate pages.

Nothing is copied from the vendors' documentation or interfaces; screen names below were read from the public documentation and are summarized in own words. The Rustryx UI is a new design.

This inventory is the page list and navigation source that [ui-design-brief.md](ui-design-brief.md) refers to. The brief fixes the stack (React, TypeScript, Vite, Tailwind, shadcn/ui, TanStack), the delivery (static SPA embedded in the `rustryx` binary), the cookie session auth and the visual rules; this document does not repeat them. UI milestones M1 to M3 below stage the work the brief calls wave 3; the backend work they depend on is wave 2 in the parity document.

## 1. What the two products show their users

Observed in the compat documentation (UI overview, browse, search, upload, administration pages):

- Left navigation: Dashboard, Search, Browse, Upload, Tags (Pro), Malware Risk (Pro), Settings; items depend on privileges. Header: logo with version, keyword search box, global refresh, help menu, account menu, a system health indicator.
- Browse: a filterable repository table (type, format, status, URL, health, firewall status), a searchable content tree per repository, and a details pane with component and asset information.
- Search: quick keyword search plus advanced criteria (format, repository, group, name, version, checksum, format-specific fields), result list with a detail pane.
- Upload: a format-specific form for hosted repositories.
- Settings: repositories, blob stores, cleanup policies, content selectors, routing rules; security (privileges, roles, users, anonymous access, realms, LDAP, SAML, SSL certificates, user tokens, service account tokens, IP allow list); system (capabilities, email, HTTP proxy, tasks, nodes, data store, API explorer, licensing); support (status, logs, system information, support ZIP, metrics).
- A newer interface runs next to the classic one with a switch; admins can control who sees it.

Observed in the Harbor documentation:

- Projects list; inside a project: repositories, artifacts (digest list with type, annotations, push time, pull command), artifact details (tags, vulnerabilities, accessories such as signatures and SBOM), labels, scanner choice, P2P preheat policies, robot accounts, webhooks, logs, configuration (visibility, pull gate, auto scan, SBOM generation, CVE allowlist, content trust, retention, immutability).
- Administration: users, robot accounts, labels, project quotas, registries and replications (rules, executions, task logs), interrogation services (scanners, vulnerability schedule, scan all), Security Hub, clean up (garbage collection with dry run, audit log purge), job service dashboard, configuration (authentication, system settings with banner and read-only mode, security allowlist).
- User profile with a CLI secret; an API explorer reachable from the navigation.

## 2. Merge decisions

| Decision | Reason |
|---|---|
| Browse stays repository-centric (compat), projects are an optional grouping layer on top (Harbor) | Rustryx repositories are format typed; a project groups repositories, members and policies (parity doc 11.1) without hiding the repository view |
| One global search and one global upload, reachable from the header | both are first-class in compat; Harbor users search by project, which is a filter |
| OCI artifacts get a dedicated artifact page; every other format uses the component page | OCI has digests, tags, accessories and indexes that do not fit a path tree |
| Administration is one area with five groups: storage, identity and access, automation, security and compliance, system | merges compat Settings and Harbor Administration without duplicating entries |
| Per-project policies live under the project; global defaults and engines live under Administration | matches who may change what (project admin vs `nx-admin`) |
| `client setup` is a panel reused in four places | removes per-format pages (maven, npm, pip, docker, helm, pub, raw) |
| No generic capabilities page | parity doc 12.4: typed settings pages instead |
| No in-UI application log viewer, no support ZIP page in the first milestones | stateless nodes with stdout logs (parity doc `OPS-13`, `OPS-21`) |

## 3. Design principles

1. Privilege-aware navigation: entries the user cannot use are hidden, not disabled; a direct link to a forbidden page shows a clear no-access state (not a blank page).
2. Every list is server-paginated with cursor tokens, sortable, filterable, and its state is kept in the URL so views can be shared and bookmarked.
3. Every destructive action names its consequences first (dependency list or dry-run report) and asks for typed confirmation when it cannot be undone.
4. Secrets (tokens, robot secrets, webhook secrets) are shown once in a dedicated dialog with copy and download, never stored in the browser, never returned by the API afterwards.
5. Long operations (GC, scans, replication, lifecycle runs, export) return a run object; the UI shows a toast with a link to the run page and keeps polling cheaply.
6. Keyboard and accessibility first: full keyboard navigation, visible focus, contrast 4.5:1 in both themes, status never by colour alone, dark and light themes, density setting, responsive at 375, 768, 1024 and 1440 px without horizontal page scroll (rules in the design brief).
7. English first, with every string in a message catalog from day one so a Russian catalog can follow without code changes (the specification is in Russian; the Russian catalog is not a gate for M1).
8. Concurrency: updates send the object version (`If-Match`); a conflict shows a three-way choice (reload, overwrite, copy my changes).
9. Command palette on every page (Ctrl or Cmd plus K): jump to a repository, search components, open an administration page; it lists only what the user may open.

## 4. Privilege vocabulary used in the tables

| Notation | Meaning |
|---|---|
| `anon` | no login; only public repositories and projects, read |
| `user` | any authenticated principal |
| `repo.browse(R)`, `repo.read(R)`, `repo.write(R)`, `repo.delete(R)` | repository view actions on repository R (or `*`); browse is UI and search, read is client download (`SRCH-11`) |
| `repo.admin(R)` | edit the configuration of repository R (phase 1: only `nx-admin`) |
| `project.<role>(P)` | project role on P: `limited-guest`, `guest`, `developer`, `maintainer`, `project-admin` (parity doc 11.1) |
| `app.<area>.read` and `app.<area>.write` | application privileges per administration area: `users`, `roles`, `privileges`, `selectors`, `mappings`, `tokens`, `blobstores`, `tasks`, `settings`, `audit`, `scanners`, `registries`, `replication`, `webhooks`, `quotas`, `gc`, `system` |
| `nx-admin` | implies every `app.*` privilege and every project role |

In phase 1 the admin pages need `nx-admin`; per-area application privileges arrive with `AUTH-46`. compat privilege names seen in the docs (for orientation only): `nx-users`, `nx-roles`, `nx-tasks`, `nx-settings`, `nx-capabilities`, `nx-atlas`, `nx-metrics-all`, `nx-component-upload`, `nx-search-read`, `nx-ssl-truststore`.

## 5. Shared states and patterns (designed once, referenced by every page)

| State | Design |
|---|---|
| Loading | table and tree skeleton rows after 150 ms; stale data stays visible during refresh (stale-while-revalidate); buttons show progress only after 300 ms |
| Empty, first use | illustration-free message, one primary call to action (for example "Create repository"), link to docs |
| Empty, filtered | "No results for these filters" with a clear-filters button and the active filter chips |
| 401 | session expired dialog, return to the same URL after login |
| 403 | page level "You do not have access" with the missing privilege name and who can grant it; actions hidden |
| 404 | "Not found or hidden from you" (the API answers 404 for hidden repositories, 01-architecture 1.5) |
| 409 | inline conflict text with the blocking object linked (for example a routing rule still assigned) |
| 422 | field-level errors mapped from the problem document; the first error receives focus |
| 429 | countdown from `Retry-After`, then automatic retry for reads |
| 5xx and 503 | message with the stable error `code` and the `request_id`, each with a copy button; for 503 the page keeps showing last data and a retry button |
| Identity provider down | login page offers local login and states that SSO is unavailable (README 8.3); signed-in sessions are unaffected |
| System read-only | persistent banner; all write actions disabled with a tooltip naming the cause (`OPS-22`) |
| Degraded status | banner from `/api/v1/system/status` (for example blob store over quota, default credentials) visible to admins |
| Node draining | banner when a request returns the drain response; retry hits another node |
| Long run | toast with run link; run page shows progress, log tail, stop button, final result |
| Secret shown once | modal with copy, download, "I stored it" checkbox that enables close |
| Destructive | dependency list, dry-run report when available, typed name for irreversible actions |

## 6. Navigation tree

Milestones in brackets: M1, M2, M3 (section 11). Pages without a bracket inherit the milestone of their parent.

```
Shell (all pages): header with global search, command palette, upload shortcut, jobs indicator, theme,
                   user menu; left navigation below; banners (read-only, degraded, SSO down)
|
+-- Home (dashboard)                                        [M1]
+-- Repositories                                            [M1]
|   +-- Repository list, create wizard
|   +-- <repository>
|       +-- Overview (summary, client setup panel)
|       +-- Browse (tree and detail pane)
|       +-- Components (flat table)
|       +-- Settings (configuration, attached cleanup policies)
|       +-- Activity (audit events of the repository)
|       +-- Health (vulnerability summary)                  [M2]
|   +-- Component detail, Asset detail
|   +-- OCI image and artifact pages                        [M1 basic, M2 full]
+-- Search                                                  [M1]
+-- Upload                                                  [M1]
+-- Projects                                                [M2]
|   +-- Project list, create
|   +-- <project>: Summary, Repositories, Members, Labels, Policies (retention, immutability),
|                  Security (scanner, pull gate, allowlist, trust, SBOM), Robot tokens,
|                  Webhooks, Logs, Settings
+-- Security                                                [M2]
|   +-- Overview (Security Hub), Vulnerability search, Scan activity
+-- Administration                                          [M1 unless marked]
|   +-- Content: Cleanup policies, Routing rules, Content selectors
|   +-- Storage: Blob stores, Garbage collection [M2]
|   +-- Identity and access: Users, Roles, Privileges, Group mappings,
|                            Authentication providers, Anonymous access, API tokens (all)
|   +-- Automation: Tasks, Webhooks [M2], Registries [M2], Replication [M2], Jobs [M3]
|   +-- Security and compliance: Scanners [M2], CVE allowlist [M2], Trust policies [M2], Quotas [M2]
|   +-- System: Settings, Email [M2], HTTP proxy [M2], Certificates [M2], Audit log,
|               Status, Nodes [M2], Info [M2], Log levels [M3], Backup and export [M3],
|               Migration [M3], Usage [M3], IP allow list [M3]
+-- Account (user menu): Profile, Personal tokens, Preferences, Sign out   [M1]
+-- Help: API explorer [M3], About and licenses [M1]
```

## 7. Page inventory: shell, browse, search, upload

Common columns: API resources are under `/api/v1/` (resource names only). Privileges use the vocabulary of section 4. States list what is specific to the page, on top of section 5.

### 7.1 Sign in and home

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Sign in `/login` | local username and password form; one button per configured SSO provider; break-glass note for the local admin; return-to URL | `auth/providers` (public list), session endpoints of the auth service (code flow callback is server-side), `me` | `anon` | wrong credentials (generic message, no user enumeration); locked out with `Retry-After` countdown (login guard, `AUTH-27`); SSO unavailable (button disabled, local form stays); session expired return | M1 |
| Dashboard `/` | role-aware landing: repository count and health strip, storage usage by blob store, recent activity (audit), failed tasks, quick actions (create repository, upload, create token); anonymous visitors see public repositories only | `repositories`, `blob-stores` (usage), `audit-events` (recent), `tasks` (failed runs), `system/status` | `user` (widgets filtered by privilege), `anon` for the public list | no repositories yet (setup checklist: create blob store, create repository, set base URL, create token); widget error does not break the page | M1 |

### 7.2 Repositories

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Repository list `/repositories` | filterable, sortable table: name, format, type, online, blob store, size, component count, health badge (M2), URL with copy; row actions: open, client setup drawer, edit, delete; bulk filters by format and type | `repositories` | `repo.browse(R)` per row; actions need `repo.admin(R)` or `nx-admin` | empty (wizard CTA for admins, "no repositories visible to you" for others); filtered empty; large list (virtual scroll) | M1 |
| Create repository `/repositories/new` | wizard: format, then type; hosted: blob store, write policy, strict content type, cleanup policies, anonymous flag; proxy: remote URL (SSRF feedback), upstream credentials via secret reference, component and metadata age, negative cache TTL, auto-block, routing rule, registry endpoint (M2), namespace allowlist for OCI; group: ordered member list with drag and drop and a preview of the merge order; format-specific blocks (Maven version and layout policy, Docker/OCI options, pub); review step | `repositories`, `blob-stores`, `cleanup-policies`, `routing-rules`, `registries` (M2), `formats` (schema of format options) | `repo.admin(*)` or `nx-admin` | name taken (409, live check); remote URL rejected by SSRF filter (explain which rule); proxy test connection failed (warn, allow save); no blob store (link to create one) | M1 |
| Repository overview `/repositories/:name` | summary header (format, type, online switch, URL, blob store, write policy); cards: storage and count, health summary (M2), recent components, activity; the `client setup` panel; for groups the member list with order; for proxies the upstream status (blocked, auto-blocked, last error) | `repositories`, `components` (recent), `audit-events`, `client-setup` | `repo.browse(R)` | offline repository (banner and why); proxy upstream down (status chip and serve-stale note); group with an offline member | M1 |
| Browse `/repositories/:name/browse?path=` | lazy tree (folders first, virtualized) beside a detail pane; breadcrumb path; filter box (prefix); pane shows component or asset detail with actions (download, copy URL, delete, copy to, label); group repositories show source member per item | `repositories/{name}/tree`, `components`, `assets` | `repo.browse(R)`; delete needs `repo.delete(R)` | empty repository (shows how to publish with the client setup panel); folder with thousands of items (paged, "load more"); item removed meanwhile (404 toast, tree refresh); selector-hidden folder never listed | M1 |
| Components `/repositories/:name/components` | flat table with filters (name, group, version, label, tagged state for OCI); column chooser; multi-select for delete or label (M2); export CSV of the filtered list (M2) | `components`, `search/components` | `repo.browse(R)` | same as list; OCI shows image name and tag count instead of group and version | M1 |
| Repository settings `/repositories/:name/settings` | same form as create, with fields that cannot change after creation locked and explained; attached cleanup policies picker; proxy actions (invalidate cache, block); danger zone (delete with dependency list) | `repositories`, `cleanup-policies`, `routing-rules`, `repositories/{name}/actions/invalidate-cache` | `repo.admin(R)` or `nx-admin` | version conflict dialog; delete blocked while referenced by a group (409 with list); unsaved-changes guard | M1 |
| Repository activity `/repositories/:name/activity` | audit events filtered to this repository: who published, deleted, changed settings | `audit-events?repository=` | `repo.admin(R)` or `app.audit.read` | no events in range; long ranges paged | M1 |
| Repository health `/repositories/:name/health` | severity histogram, top findings, last scan time, enable switch, scan now | `repositories/{name}/health`, `scans`, `vulnerabilities` | `repo.browse(R)`; switch needs `repo.admin(R)` | scanner not configured (admin link); never scanned; scan failed with log link | M2 |
| Component detail `/components/:id` | header (name, version, repository, labels), assets table, attributes, age, vulnerabilities and SBOM tabs (M2), accessories (M2), actions: delete, copy or move to, add label | `components`, `labels`, `vulnerabilities`, `sboms`, `promotions` | `repo.browse(R)`; actions need write or delete | component deleted meanwhile; immutable (actions disabled, rule named); quota exceeded on copy (error with numbers) | M1 basic, M2 full |
| Asset detail `/assets/:id` | summary (path, content type, size, blob created and updated, last downloaded, download count, uploader, blob reference) and attributes (checksums, format attributes); download and copy URL | `assets` | `repo.browse(R)`; download needs `repo.read(R)` | uploader hidden when privacy setting says so; asset missing in blob store (integrity warning, link to `verify-blobs` task for admins) | M1 |
| OCI image page `/repositories/:name/images/:image` | tag table (tag, digest, size, push time, signed badge, vulnerability badge) with search, untagged filter, label filter; actions: copy pull command, retag, delete tag, delete artifact; repository description (Markdown) | `components`, `labels`, `components/{id}/accessories` | `repo.browse(R)`; push-side actions need `repo.write(R)` | image with a single untagged digest; multi-arch index row expands to children (read only); immutable tag shows a lock and the rule | M1 basic, M2 full |
| OCI artifact page `/repositories/:name/artifacts/:digest` | manifest and config view, layers, build history, tags, accessories (signatures, SBOM) as a table with delete, vulnerabilities (summary and table), labels, scan and SBOM actions, replication status (M2) | `components`, `components/{id}/config`, `components/{id}/accessories`, `scans`, `vulnerabilities`, `sboms`, `labels` | `repo.browse(R)`; scan and delete need project maintainer or `repo.write(R)` | scan running, failed, not scanned; scanner limits (signature artifacts are not scanned); trust policy denies pull (explain which check failed) | M2 |

### 7.3 Search and upload

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Search `/search?q=&format=&repo=&group=&name=&version=&sha256=` | keyword box with the same box in the header; criteria chips (format, repository, group, name, version, checksum, label); format-specific criteria that appear with a chosen format (Maven coordinates, Docker tag and digest, others in M2); result table with the detail pane from Browse; link to the repository | `search/components`, `search/assets`, `repositories` (for the repository filter), `labels` | `anon` or `user`; results are filtered by `repo.browse(R)` | no query yet (recent and example searches); no results (suggest wildcard rules and minimum prefix); query too short (hint); very many results (capped list with "refine" hint) | M1 |
| Upload `/upload` | choose a hosted repository, then a form built from the format upload schema: file drop area (multi-file for Maven and raw), coordinates, extra attributes, label picker (M2); progress per file with resume-less retry; result summary with links to the new component | `repositories`, `formats/{format}/upload-schema`, `components` (POST multipart), `labels` | `repo.write(R)` | repository is not hosted or format cannot be uploaded in the UI (explains and points to the client setup panel); write policy forbids redeploy (409 with the policy name); quota exceeded; checksum mismatch; upload interrupted (partial file removed by the server, retry offered) | M1 |
| Client setup `panel` (not a page) | see section 9 | `client-setup` | `repo.browse(R)` | see section 9 | M1 |

## 8. Page inventory: projects, security, administration, account

### 8.1 Projects (Harbor-style layer)

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Project list `/projects` | table: name, visibility, repositories, members, artifact count, storage usage against quota, vulnerability summary, last push; create button | `projects`, `quotas` | `user` (sees own projects and public ones); create needs the creation policy (`OPS-24`) | no projects (create CTA or "ask an administrator" depending on policy); quota near limit (amber), exceeded (red) | M2 |
| Create project `/projects/new` | name, visibility, description, repositories to attach or create (OCI hosted by default), optional proxy repository bound to a registry endpoint, quota (admin only) | `projects`, `repositories`, `registries`, `quotas` | creation policy or `nx-admin` | name taken; invalid name rules shown live; proxy upstream rejected by SSRF filter | M2 |
| Project summary `/projects/:p` | counts, quota usage bar, vulnerability severity histogram, recent pushes, members glance, client setup panel for the project repositories | `projects`, `quotas`, `vulnerabilities`, `audit-events` | `project.limited-guest(P)` or public | no data yet; scanner not configured | M2 |
| Project repositories `/projects/:p/repositories` | repositories attached to the project with their types; attach existing, create new; OCI image counts | `projects/{id}/repositories`, `repositories` | `project.guest(P)`; attach and create need `project.maintainer(P)` or higher | repository already in another project (409) | M2 |
| Project members `/projects/:p/members` | table of users and identity-provider groups with project role; add member (user search or group path), change role, remove; shows effective source (manual, mapped from group) | `projects/{id}/members`, `users`, `idp-group-mappings` | read: `project.limited-guest(P)`; write: `project.project-admin(P)` | removing the last project admin (blocked with explanation); group path unknown to the identity provider (accept with warning) | M2 |
| Project labels `/projects/:p/labels` | label list with colors, create and edit; usage counts; global labels shown read-only first | `labels` | read: `project.guest(P)`; write: `project.project-admin(P)` | label in use on delete (shows count) | M2 |
| Project policies `/projects/:p/policies` | two tabs. Retention: rule list editor (templates, repository and tag selectors with matching or excluding mode, untagged option), schedule picker, dry-run and run now, history. Immutability: rule list editor with selector tester ("would this tag be protected?"), enable toggles | `retention-policies`, `immutability-rules`, `lifecycle-runs`, `tasks` | read: `project.guest(P)`; retention write: `project.developer(P)` and above (Harbor matrix); immutability write: `project.maintainer(P)` and above | no rules yet; rule limit reached; dry-run report with large lists (paged, CSV); run in progress; protected-by-immutability rows marked in the report | M2 |
| Project security `/projects/:p/security` | scanner choice (or system default), scan on push, pull gate severity threshold, project CVE allowlist with expiry and copy-from-system, trust policy (cosign, notation, both) with trust material, SBOM on push | `scanners`, `cve-allowlists`, `trust-policies`, `projects` | read: `project.guest(P)`; write: `project.project-admin(P)` | no scanner available; allowlist expired entries highlighted; trust policy missing keys (blocking validation, no presence-only mode) | M2 |
| Project robot tokens `/projects/:p/robots` | token table (name, scopes summary, expiry, last used, enabled); create dialog with a scope picker (repository selector and action matrix, push implies pull), expiry, show-once secret dialog; rotate, disable, delete | `api-tokens?kind=robot&project=` | read: `project.maintainer(P)`; write: `project.project-admin(P)` | scope exceeds the creator's privileges (field error naming the missing privilege); token expired (row greyed with renew); rotation warning that CI will fail until updated | M2 |
| Project webhooks `/projects/:p/webhooks` | webhook list, create (URL, kind, auth header or secret, event checklist, enabled), test delivery, delivery history with status and response | `webhooks`, `webhook-deliveries` | read: `project.maintainer(P)`; write: `project.project-admin(P)` | URL rejected by SSRF filter; delivery failing (status chip, last error); system-wide switch off (banner) | M2 |
| Project logs `/projects/:p/logs` | audit events of the project: pull, push, delete, config changes; filters for operation, user, resource, date | `audit-events?project=` | `project.guest(P)` (limited guests cannot see logs, per the Harbor matrix) | no events; large date range | M2 |
| Project settings `/projects/:p/settings` | visibility, description, quota (admin only), delete project (dependency list, typed name) | `projects`, `quotas` | `project.project-admin(P)`; quota: `nx-admin` | delete blocked while repositories are non-empty | M2 |

### 8.2 Security center

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Security overview `/security` | registry-wide totals by severity, scanned vs total artifacts, top 5 artifacts, top 5 CVEs, scan queue health | `vulnerabilities` (aggregates), `scans` | `app.scanners.read` or `nx-admin` | no scanner; scans never run; data older than the stale threshold (warning) | M2 |
| Vulnerability search `/security/vulnerabilities?cve=` | search by CVE id (required) with filters for severity, score range, project, repository, digest, package, tag; results table with links to artifacts; CSV export | `vulnerabilities`, `vulnerabilities/exports` | `app.scanners.read` or `nx-admin`; project-scoped view for project maintainers | CVE id missing (explains the performance reason); no matches; export running, ready once, then removed | M2 |
| Scan activity `/security/scans` | running, queued and failed scans with logs; scan all, stop all; schedule shortcut | `scans`, `tasks` | `app.scanners.write` or `nx-admin` | queue empty; long queue (age of oldest); scan all disabled while running | M2 |

### 8.3 Administration: content and storage

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Cleanup policies `/admin/cleanup-policies` | list with format, criteria summary, attached repositories, last run; editor with criteria that depend on the format, preview button that starts a dry run and shows the stored report | `cleanup-policies`, `lifecycle-runs`, `repositories` | `nx-admin` (later `app.tasks.write`) | no policies; preview too large (report is paged and exported as CSV, not shown inline); criterion not supported for the chosen format (disabled with reason) | M1 |
| Routing rules `/admin/routing-rules` | list with mode and patterns, assigned repositories; editor with a path tester (enter paths, see allow or block and the matching pattern); pattern linter that rejects constructs the engine does not support | `routing-rules`, `routing-rules/{id}/test` | `nx-admin` | delete blocked while assigned (409, repositories linked); invalid pattern (position of the error) | M1 |
| Content selectors `/admin/content-selectors` | list; editor for the expression with syntax help and variables (`format`, `path`, `repository`, `coordinate.*`); preview against a chosen repository (10 results) | `content-selectors`, `content-selectors/preview` | `app.selectors.write` or `nx-admin` | expression error with location; evaluation limit hit (SEC-18 explanation); selector in use by privileges (list on delete) | M1 |
| Blob stores `/admin/blob-stores` | table: name, kind (local, S3, Azure, group), blob count, used and free space, health state, quota status, repositories using it; create wizard by kind | `blob-stores`, `blob-stores/{name}/usage` | `app.blobstores.read`; write: `nx-admin` | store unreachable (red chip with last error); over soft quota (amber, no blocking text); empty (create CTA) | M1 |
| Blob store detail `/admin/blob-stores/:name` | configuration (secrets as references, test connection), members and fill policy for groups (M2), soft quota, usage charts, repositories using it, danger zone | `blob-stores`, `blob-stores/{name}/members` | `app.blobstores.write` or `nx-admin` | cannot delete while referenced (409 with list); convert to group confirmation (irreversible) (M2) | M1, groups M2 |
| Garbage collection `/admin/gc` | schedule, options (delete untagged, workers, minimum age shown read only), Run now and Dry run buttons, history table with trigger, dry-run flag, status, reclaimed estimate, log link, stop; dry-run report viewer (counts, bytes per blob store, sample list) | `gc/runs`, `tasks` | `app.gc.write` or `nx-admin` | no runs yet; run in progress (progress, stop); manual run limit (once per minute message); stop leaves already removed blobs (explicit note) | M2 |

### 8.4 Administration: identity and access

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Users `/admin/users` and `/admin/users/:id` | list filtered by source (local, LDAP, OIDC), status, role; detail shows identity (source, issuer and subject for OIDC), effective roles with origin (manual, mapped from group), groups seen at last login, last login, tokens, status switch; local users: create, reset password, disable | `users`, `roles`, `api-tokens` | `app.users.read`; write: `nx-admin` | external user fields read only (with reason); username clash across sources (409 explanation, README 8.5); roles from groups cannot be removed here (explain) | M1 |
| Roles `/admin/roles` | list with nesting; editor with privileges picker and nested roles picker (cycle check), built-in roles locked | `roles`, `privileges` | `app.roles.read`; write: `nx-admin` | built-in role (read only badge); cycle detected; role in use on delete | M1 |
| Privileges `/admin/privileges` | list by kind (repository view, content selector, repository admin, application); creation forms per kind; shows which roles use each | `privileges`, `content-selectors`, `repositories` | `app.privileges.read`; write: `nx-admin` | privilege without actions (blocked); wildcard repository warning | M1 |
| Group mappings `/admin/group-mappings` | table of identity-provider group (Keycloak full path or LDAP DN), role or project role, priority, source; add and edit; a tester that takes a group list and shows the resulting roles; warning that `nx-admin` only comes from the configured admin groups (README 8.5) | `idp-group-mappings`, `roles`, `projects` | `app.mappings.write` or `nx-admin` | unknown group accepted with a warning; mapping to `nx-admin` refused with an explanation; duplicate mapping | M1 |
| Authentication providers `/admin/auth/providers` | tabs: local (policy), LDAP servers (list, order, test and verify buttons), OIDC provider (issuer, client id, secret reference, scopes, claim names, admin groups, default role, redirect URI display), remote user token (M2) | `auth/providers`, `auth/providers/ldap/{id}/verify` | `nx-admin` | test failed with a safe error trace (no secrets); TLS verification failure explained, no skip option; unsaved secret references | M1 read only for file config, M2 editable |
| Anonymous access `/admin/anonymous` | global switch, per-repository list of `allow_anonymous` with bulk change, effective result column (both switches must agree) | `anonymous-access`, `repositories` | `nx-admin` | global off (list shows all as effectively private); enabling shows the warning about public exposure | M1 |
| API tokens (all) `/admin/tokens` | all tokens: owner, kind, project, scopes, expiry, last used, state; revoke; filters for expired and never-used; policy section (default and maximum lifetime, require tokens for tool access) | `api-tokens`, `settings/security` | `app.tokens.read`; write: `nx-admin` | token owner deleted; many expired tokens (bulk purge) | M1 list, M2 policy |
| Realm order and IP allow list `/admin/auth/realms`, `/admin/ip-allowlist` | ordered list of identity sources; allow list with monitor or enforce | `auth/realms`, `ip-allowlist` | `nx-admin` | enforce mode lock-out warning with a proposed own-IP entry | M3 |

### 8.5 Administration: automation

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Tasks `/admin/tasks` | table: name, kind, schedule, enabled, last result, next run; create, edit, enable, run now, stop; system tasks marked | `tasks` | `app.tasks.read`; write: `nx-admin` | no user tasks (system tasks still listed); lock held by another node (run queued explanation) | M1 read only, M2 editable |
| Task run `/admin/tasks/:id/runs/:run` | status, timing, node, summary, dry-run flag, log viewer with follow, stop button | `tasks/{id}/runs` | `app.tasks.read` | log purged by retention (message with date); run lost its lock (shown as aborted) | M1 |
| Webhooks (system) `/admin/webhooks` | system-scope webhooks, same editor and delivery history as the project page; event catalog help | `webhooks`, `webhook-deliveries` | `app.webhooks.write` or `nx-admin` | SSRF rejection; replay window explanation next to the signature help | M2 |
| Registries `/admin/registries` | table: name, provider, URL, status, last test; create and edit (credential reference, CA reference, verify certificate on), test connection | `registries` | `app.registries.write` or `nx-admin` | test failed; in use on delete (list of rules and proxies) | M2 |
| Replication `/admin/replication` | rules table (direction, filters, trigger, last run) with editor; executions list per rule with task counts; run now; execution page with tasks, logs and stop | `replication-rules`, `replication-executions` | `app.replication.write` or `nx-admin` | rule edit blocked while running (explain); event trigger on signed artifacts waits for referrers (info); target incompatible version | M2 |
| Jobs `/admin/jobs` | queue view: pending per kind, oldest age, workers, schedules, pause and resume (later) | `tasks` (queue view) | `nx-admin` | read only first | M3 |

### 8.6 Administration: security and compliance

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| Scanners `/admin/scanners` | scanner list (Trivy, OSV, external HTTP), health, default marker, capabilities (vulnerability, SBOM), add external scanner with auth type, test connection; offline data status and last update | `scanners` | `app.scanners.write` or `nx-admin` | scanner unhealthy; offline database older than threshold; online OSV mode off by default with an explanation | M2 |
| CVE allowlist (system) `/admin/cve-allowlist` | CVE ids with optional expiry; add multiple ids, remove, copy count to projects; expiry highlighting | `cve-allowlists` | `app.scanners.write` or `nx-admin` | invalid id format; expiry passed | M2 |
| Trust policies `/admin/trust-policies` | system defaults and an overview of project policies; trust material management (cosign keys and identities, notation trust stores) | `trust-policies` | `nx-admin` | key expired or malformed; policy with no material (blocked) | M2 |
| Quotas `/admin/quotas` | global default, table of projects with usage and limit, edit limit, recount button | `quotas`, `projects` | `app.quotas.write` or `nx-admin` | recount running; usage mismatch warning after recount | M2 |

### 8.7 Administration: system, and account

| Page and route | Purpose and main components | API resources | Privilege | States to design | Milestone |
|---|---|---|---|---|---|
| System settings `/admin/settings` | sections as forms generated from the settings schema: general (read-only mode switch, project creation policy, banner text later), security (token defaults, session information read only), storage (last-downloaded interval) | `settings/{section}`, `settings/schema` | `app.settings.read`; write: `nx-admin` | read-only mode toggle needs a confirmation with the effect; invalid combination (422); change audited banner | M1 basic, M2 full |
| Email `/admin/settings/email` | SMTP host, port, TLS mode, user, password reference, sender; verify (send test mail), disable | `settings/email`, `settings/email/verify` | `nx-admin` | verify failed with a safe error; not configured banner on pages that need mail | M2 |
| HTTP proxy `/admin/settings/http-proxy` | outbound proxy host and port, credentials reference, exclusion list, user-agent suffix, timeouts shown read only | `settings/http-proxy` | `nx-admin` | exclusion pattern preview; change affects all upstreams (warning) | M2 |
| Certificates `/admin/settings/certificates` | trusted CA list with subject, expiry and usage; upload PEM; remove; per-upstream CA assignment is done in repository settings | `trusted-certificates` | `nx-admin` | certificate near expiry (amber); removal still used (409 with list) | M2 |
| Audit log `/admin/audit` | filter bar (event type, user, resource, repository, project, date range), table with expandable JSON detail, NDJSON export, retention info | `audit-events` | `app.audit.read` or `nx-admin` | no events in range; export of a huge range runs as a job; partition older than retention (explain) | M1 |
| Status `/admin/system/status` | list of checks with severity and fix hints (default credentials, anonymous on, tokens never expiring, blob store quota, scanner health, read-only) and the readiness details | `system/status` | `app.system.read` or `nx-admin` | all green (compact view); a check that cannot run (unknown state, not green) | M1 |
| Nodes `/admin/system/nodes` | node table (id, version, started, heartbeat, draining), leader of each singleton task | `system/nodes` | `nx-admin` | single node setup (explains HA is optional); node heartbeat stale | M2 |
| Info `/admin/system/info` | version, build, schema version, configuration summary with secrets redacted, download as JSON | `system/info` | `nx-admin` | redaction note | M2 |
| Log levels `/admin/system/loggers` | list of loggers with level per node, change level with auto-reset timer | `system/loggers` | `nx-admin` | per-node scope warning | M3 |
| Backup and export `/admin/backup` | export and import repository tasks with progress, backup verification report, links to the runbook | `exports`, `imports`, `backups` | `nx-admin` | read-only mode required (explain and offer to enable); verification failed | M3 |
| Migration `/admin/migration` | wizard to import from a live compat: connect, select repositories and security objects, dry run, progress | `migrations` | `nx-admin` | source unreachable; unsupported format listed with reason | M3 |
| Usage `/admin/usage` | request and storage trends from metrics, top repositories | `usage` | `nx-admin` | metrics backend not configured | M3 |
| Profile `/me` | identity (source, groups from the last login, effective roles with origin), change password (local only), active sessions, sign out everywhere | `me` | `user` | external identity (password section replaced by an explanation); no roles (explains access request path) | M1 |
| Personal tokens `/me/tokens` | create (name, expiry, scopes preset), show-once dialog, list with last used, revoke; usage hints for the tools | `me/tokens`, `client-setup` | `user` | token expiry policy maximum applied (field hint); policy requires tokens for tools (banner) | M1 |
| Preferences `/me/preferences` | theme, language, density, default landing page, table column choices (stored in the browser) | `me` | `user` | storage unavailable (defaults, no error) | M1 |
| API explorer `/api-docs` | interactive OpenAPI view with authorize using the session or a token | `openapi.json` | `user` | spec unavailable | M3 |
| About `/about` | version, build id, supply chain information (SBOM link, signature verification hint), third-party licenses | `system/info` (public subset) | `anon` | none | M1 |

## 9. The reusable `client setup` panel

One component replaces every per-format "how to configure your client" page. It is embedded in the repository overview, the repository list row drawer, the project summary, the upload page, the search detail pane and the OCI image and artifact pages (as pull and push commands). The snippets are produced by the server from templates that live next to the format crates, so documentation and behaviour cannot drift apart.

| Aspect | Specification |
|---|---|
| Input | repository (name, format, type, anonymous flag, online) and the server base URL (`http.base_url`) |
| Output | structured snippets: client name, file or command, purpose (resolve, publish), text with placeholders for credentials |
| Tabs | Maven (settings for mirror or repository, publish block, Gradle note), npm (registry and auth lines), pip and twine (index URL, upload), Docker (login, tag, push, pull), Helm (OCI login, push, pull; classic repository add), Pub (hosted URL, token add, publish), raw (curl upload and download); further formats appear with their crates |
| Variants | anonymous read when both anonymous switches allow it; authenticated otherwise; group repositories show "resolve through the group, publish to the hosted repository" with the hosted member named |
| Credentials | placeholder `<TOKEN>`; a "Create a token for me" button opens the personal token dialog (show-once) and fills the snippets for the current page view only; nothing is stored in the browser |
| Privilege | `repo.browse(R)` to see resolve snippets; publish snippets only when the user has `repo.write(R)` |
| States | no repository selected (chooser); repository offline (warning, snippets still shown); base URL not configured (admin link, snippets use the browser origin with a warning); self-signed or private CA (shows the CA install note when the server reports a private CA); format without a template (generic URL only) |
| Accessibility | each snippet has a copy button with an announcement, a "show raw" toggle, and no content only readable by colour |
| API | `GET /api/v1/client-setup?repository=<name>` |

## 10. Reusable components

| Component | Used by | Notes |
|---|---|---|
| Data table with cursor paging, URL-synced filters, column chooser, bulk bar | every list | virtualization above 200 rows; keyboard row navigation |
| Lazy tree with detail pane | repository browse | server paging per folder; selection stays in the URL |
| Attribute and checksum panel | component and asset detail, OCI artifact | copy buttons for each checksum; secrets never appear here |
| Severity bar and histogram | repository health, project summary, artifact page, security center | colour plus text label and count, never colour alone |
| Rule list editor with a live tester | retention, immutability, routing rules, content selectors, group mappings | each editor ships a "test against this input" box, so rules are understood before they run |
| Dry-run report viewer | cleanup preview, retention, GC, promotion | summary first (counts, bytes), paged candidate list with the reason, CSV export, "run for real" button bound to this report |
| Schedule picker | tasks, GC, scans, retention, replication | presets plus cron with a human-readable next five runs |
| Scope picker | robot tokens, personal token presets | repository selector plus action matrix; push implies pull is enforced visibly |
| Show-once secret dialog | tokens, webhook secrets, robot rotation | copy, download, required acknowledgement |
| Audit event row with expandable JSON | audit log, repository activity, project logs | stable event ids, actor and request id copy buttons |
| Status chip set | repositories, blob stores, scanners, registries, tasks | ok, warning, error, unknown; text always present |
| Typed-confirmation destructive dialog | delete repository, project, blob store, token | lists dependent objects from the 409 response |
| Job toast and run page | GC, scans, replication, export, lifecycle runs | shared run model: status, progress, log tail, stop |
| Charts (storage by blob store, requests, scans over time) | dashboard, usage, security overview | line and bar only, at most 6 series, direct labels; data tables behind every chart for accessibility |
| Command palette | whole shell | Ctrl or Cmd plus K; jump to repository, component search, administration pages; filtered by privilege |

## 11. Phase split of the UI

### UI-M1 Usable core (phase 1 backend plus stream W2-A)

Pages: sign in; dashboard; repository list, create, overview, browse, components, settings, activity; component and asset detail; OCI image page (basic: tags and digests); search; upload; the `client setup` panel; administration of cleanup policies, routing rules, content selectors, blob stores (list, detail, create), users, roles, privileges, group mappings, authentication providers (read only view of the file configuration), anonymous access, all API tokens (list and revoke), tasks (read only) with run pages, audit log, system status, basic system settings; account profile, personal tokens, preferences; about.

Backend preconditions beyond phase 1 (all in W2-A): `/api/v1/` resources for these pages, the OpenAPI document, browse tree, client-setup, upload schema, audit query, system status, `/me`. Without them the UI cannot be built; they are small compared to the phase 1 core.

Definition of done for M1: every page has the states of section 5; keyboard-only pass on the main flows (sign in, find a component, upload, create a repository, create a token); one end-to-end test per flow against a real node; light and dark themes with contrast validated; English catalog complete and all strings externalized; the command palette works on every page.

### UI-M2 Harbor parity (streams W2-B to W2-E)

Pages: projects (all project pages), repository health, OCI artifact page (full), security center, scanners, CVE allowlist, trust policies, quotas, garbage collection with dry run, editable tasks, system and project webhooks, registries, replication, promotion dialogs, group blob stores, authentication provider editing and remote user token, email, HTTP proxy, certificates, nodes, info, token policy.

### UI-M3 Long tail and polish (streams W2-F to W2-H and the `later` items)

Pages: jobs queue view, log levels, backup and export, migration wizard, usage dashboard, realm order and IP allow list, API explorer, banner and branding; plus saved views, an accessibility audit with an external reviewer, and the Russian catalog review.

## 12. Coverage check: where each product screen lands

| Product screen (as seen in the docs) | Rustryx page |
|---|---|
| compat Dashboard | Home |
| compat Search | Search |
| compat Browse (repository table, tree, info panes) | Repositories list, Browse, Component and Asset detail |
| compat Upload | Upload |
| compat Tags (Pro) | Labels (project labels page, component labels, later a global labels view) |
| compat Malware Risk (Pro+) | not built (`SCN-17` is `later`) |
| compat Settings: repositories, blob stores, cleanup policies, content selectors, routing rules | Repository pages, Blob stores, Cleanup policies, Content selectors, Routing rules |
| compat Settings: privileges, roles, users, anonymous access, realms, LDAP, SAML, SSL certificates | Privileges, Roles, Users, Anonymous access, Authentication providers and realm order, Certificates; SAML is `later` |
| compat Settings: user tokens, service account tokens, IP allow list | Personal tokens, API tokens (all) and robot tokens, IP allow list (M3) |
| compat Settings: capabilities, email, HTTP, tasks, nodes, data store | typed settings pages (parity doc 12.4), Email, HTTP proxy, Tasks, Nodes; data store page is not needed (PostgreSQL only) |
| compat Support: status, logs, system information, support ZIP, metrics | Status, Info; logs viewer and support ZIP are not built; metrics via Prometheus |
| compat API explorer | API explorer (M3) |
| Harbor Projects list and project tabs: repositories, members, labels, scanner, policy, robot accounts, webhooks, logs, configuration | Projects pages (section 8.1) |
| Harbor artifact list and artifact details (tags, vulnerabilities, accessories, SBOM) | OCI image page and OCI artifact page |
| Harbor Administration: users, robot accounts, labels, quotas | Users, API tokens (all), project Labels, Quotas |
| Harbor Administration: registries and replications | Registries, Replication |
| Harbor Administration: interrogation services, Security Hub | Scanners, Security overview, Vulnerability search, Scan activity |
| Harbor Administration: garbage collection, audit log purge | Garbage collection; audit retention is partition based (no purge page) |
| Harbor Administration: job service dashboard | Jobs (M3) |
| Harbor Administration: configuration (authentication, system settings, security allowlist) | Authentication providers, System settings, CVE allowlist |
| Harbor user profile and CLI secret | Profile, Personal tokens |
| Harbor API explorer | API explorer (M3) |
| Harbor P2P preheat | not built (`UPL-15` is `later`) |

## 13. Sources and open points

- The product screens above come from the pages listed in the Sources section of [parity-compat-harbor.md](parity-compat-harbor.md): the compat UI overview, browsing, searching, uploading, administration and support pages, and the Harbor administration, project configuration, working with images and security pages. No live instance of either product was used, so exact menu labels and layout are unverified; the IA above is a Rustryx design that covers the capabilities, not a copy of the layouts.
- Open points for the product owner: the creation policy for projects (everyone or administrators), whether group repositories may accept publishes later (`REPO-45`), whether the uploader IP is shown in asset detail (privacy), and the default language of the interface.
- Dependencies on the parity document: page states for scans, trust and quotas assume the designs in sections 11.5, 11.7 and 11.10 of that document.
