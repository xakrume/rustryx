# Native API conventions (`/api/v1`)

Status: contract draft for UI-M1 and UI-M2, 2026-10-24. Companion files: [openapi/rustryx-api-v1.yaml](openapi/rustryx-api-v1.yaml) (the contract, OpenAPI 3.1) and [openapi/check.sh](openapi/check.sh) (validation). Related: [ui-page-inventory.md](ui-page-inventory.md), [parity-compat.md](parity-compat.md), [rbac.md](rbac.md), [auth-core.md](auth-core.md), [oidc.md](oidc.md), [server.md](server.md), [core-gaps.md](core-gaps.md).

The web UI (a React SPA) and the native API handlers are built in parallel against one contract. The YAML is the machine readable half (paths, schemas, examples, one `x-required-privilege` per operation); this document is the human half: the rules that hold for every operation, the error code catalogue, the authentication flow, the audit rules and a map from every UI page to its operations. Where the two disagree, fix the YAML and this document in the same change.

Contents: [Operation index](#operation-index), [1 Scope](#1-scope-and-sources), [2 URLs and naming](#2-url-and-naming-rules), [3 Versioning](#3-versioning-and-compatibility), [4 Media types and limits](#4-media-types-bodies-and-limits), [5 Errors](#5-errors), [6 Pagination](#6-pagination), [7 Filtering and sorting](#7-filtering-and-sorting), [8 ETag and If-Match](#8-concurrent-edits-etag-and-if-match), [9 Idempotency](#9-idempotency-and-retries), [10 Rate limits](#10-rate-limits-and-overload), [11 Authentication](#11-authentication-sessions-and-csrf), [12 Authorization](#12-authorization), [13 Audit](#13-audit), [14 Long running operations](#14-long-running-operations), [15 Exports](#15-exports-ndjson-and-csv), [16 Page map](#16-ui-page-to-operation-map), [17 Inventory](#17-operation-inventory), [18 Decisions](#18-decisions-where-the-documents-were-silent), [19 Conflicts](#19-conflicts-with-rustryx-core-and-rustryx-api), [20 Maintenance](#20-maintaining-the-contract).

## Operation index

Every operation of the YAML, generated from the file itself (the YAML is about 20 thousand lines, this table is the quick way to find an operation). Columns: tag (the first tag), method, path, `operationId`, `x-required-privilege`, milestone. Details, schemas and examples are in [openapi/rustryx-api-v1.yaml](openapi/rustryx-api-v1.yaml); `check.sh` fails when this document misses an `operationId` or an error code of the YAML.

| Tag | Method | Path | operationId | Privilege | M |
|---|---|---|---|---|---|
| Auth | GET | `/api/v1/auth/providers` | `getLoginProviders` | `anonymous` | M1 |
| Auth | GET | `/api/v1/auth/providers/details` | `getAuthProvidersDetails` | `nx-admin` | M1 |
| Auth | POST | `/api/v1/auth/login` | `login` | `anonymous` | M1 |
| Auth | GET | `/api/v1/auth/oidc/login` | `startOidcLogin` | `anonymous` | M1 |
| Auth | GET | `/api/v1/auth/oidc/callback` | `completeOidcLogin` | `anonymous` | M1 |
| Auth | POST | `/api/v1/auth/logout` | `logout` | `anonymous` | M1 |
| Auth | GET | `/api/v1/auth/session` | `getSession` | `anonymous` | M1 |
| Auth | POST | `/api/v1/auth/session/refresh` | `refreshSession` | `interactive` | M1 |
| Auth | GET | `/api/v1/auth/csrf` | `getCsrfToken` | `interactive` | M1 |
| Auth | PUT | `/api/v1/auth/providers/oidc` | `updateOidcProvider` | `nx-admin` | M2 |
| Auth | PUT | `/api/v1/auth/providers/ldap` | `updateLdapProvider` | `nx-admin` | M2 |
| Auth | POST | `/api/v1/auth/providers/ldap/verify` | `verifyLdapProvider` | `nx-admin` | M2 |
| Me | GET | `/api/v1/me` | `getMe` | `anonymous` | M1 |
| Me | PUT | `/api/v1/me/password` | `changeMyPassword` | `interactive` | M1 |
| Me | POST | `/api/v1/me/actions/sign-out-everywhere` | `signOutEverywhere` | `interactive` | M1 |
| Me | GET | `/api/v1/me/tokens` | `listMyTokens` | `interactive` | M1 |
| Me | POST | `/api/v1/me/tokens` | `createMyToken` | `interactive` | M1 |
| Me | POST | `/api/v1/me/tokens/{token_id}/actions/revoke` | `revokeMyToken` | `interactive` | M1 |
| Me | GET | `/api/v1/me/sessions` | `listMySessions` | `interactive` | M2 |
| Me | DELETE | `/api/v1/me/sessions/{session_id}` | `revokeMySession` | `interactive` | M2 |
| API tokens | GET | `/api/v1/api-tokens` | `listApiTokens` | `nx-admin` | M1 |
| API tokens | GET | `/api/v1/api-tokens/{token_id}` | `getApiToken` | `nx-admin` | M1 |
| API tokens | POST | `/api/v1/api-tokens/{token_id}/actions/revoke` | `revokeApiToken` | `nx-admin` | M1 |
| Users | GET | `/api/v1/users` | `listUsers` | `nx-admin` | M1 |
| Users | POST | `/api/v1/users` | `createUser` | `nx-admin` | M1 |
| Users | GET | `/api/v1/users/{user_id}` | `getUser` | `nx-admin` | M1 |
| Users | PATCH | `/api/v1/users/{user_id}` | `patchUser` | `nx-admin` | M1 |
| Users | DELETE | `/api/v1/users/{user_id}` | `deleteUser` | `nx-admin` | M1 |
| Users | PUT | `/api/v1/users/{user_id}/roles` | `setUserRoles` | `nx-admin` | M1 |
| Users | POST | `/api/v1/users/{user_id}/actions/reset-password` | `resetUserPassword` | `nx-admin` | M1 |
| Users | POST | `/api/v1/users/{user_id}/actions/sign-out` | `signOutUser` | `nx-admin` | M1 |
| Privileges | GET | `/api/v1/privileges` | `listPrivileges` | `nx-admin` | M1 |
| Privileges | POST | `/api/v1/privileges` | `createPrivilege` | `nx-admin` | M1 |
| Privileges | GET | `/api/v1/privileges/{privilege_id}` | `getPrivilege` | `nx-admin` | M1 |
| Privileges | PUT | `/api/v1/privileges/{privilege_id}` | `updatePrivilege` | `nx-admin` | M1 |
| Privileges | DELETE | `/api/v1/privileges/{privilege_id}` | `deletePrivilege` | `nx-admin` | M1 |
| Roles | GET | `/api/v1/roles` | `listRoles` | `nx-admin` | M1 |
| Roles | POST | `/api/v1/roles` | `createRole` | `nx-admin` | M1 |
| Roles | GET | `/api/v1/roles/{role_id}` | `getRole` | `nx-admin` | M1 |
| Roles | PUT | `/api/v1/roles/{role_id}` | `updateRole` | `nx-admin` | M1 |
| Roles | DELETE | `/api/v1/roles/{role_id}` | `deleteRole` | `nx-admin` | M1 |
| Roles | GET | `/api/v1/roles/{role_id}/effective-privileges` | `getRoleEffectivePrivileges` | `nx-admin` | M1 |
| Content selectors | GET | `/api/v1/content-selectors` | `listContentSelectors` | `nx-admin` | M1 |
| Content selectors | POST | `/api/v1/content-selectors` | `createContentSelector` | `nx-admin` | M1 |
| Content selectors | POST | `/api/v1/content-selectors/validate` | `validateContentSelector` | `nx-admin` | M1 |
| Content selectors | POST | `/api/v1/content-selectors/test` | `testContentSelector` | `nx-admin` | M1 |
| Content selectors | POST | `/api/v1/content-selectors/preview` | `previewContentSelector` | `nx-admin` | M1 |
| Content selectors | GET | `/api/v1/content-selectors/language` | `getSelectorLanguage` | `nx-admin` | M1 |
| Content selectors | GET | `/api/v1/content-selectors/{selector_id}` | `getContentSelector` | `nx-admin` | M1 |
| Content selectors | PUT | `/api/v1/content-selectors/{selector_id}` | `updateContentSelector` | `nx-admin` | M1 |
| Content selectors | DELETE | `/api/v1/content-selectors/{selector_id}` | `deleteContentSelector` | `nx-admin` | M1 |
| IdP group mappings | GET | `/api/v1/idp-group-mappings` | `listIdpGroupMappings` | `nx-admin` | M1 |
| IdP group mappings | POST | `/api/v1/idp-group-mappings` | `createIdpGroupMapping` | `nx-admin` | M1 |
| IdP group mappings | POST | `/api/v1/idp-group-mappings/test` | `testIdpGroupMappings` | `nx-admin` | M1 |
| IdP group mappings | PUT | `/api/v1/idp-group-mappings/{mapping_id}` | `updateIdpGroupMapping` | `nx-admin` | M1 |
| IdP group mappings | DELETE | `/api/v1/idp-group-mappings/{mapping_id}` | `deleteIdpGroupMapping` | `nx-admin` | M1 |
| Anonymous access | GET | `/api/v1/anonymous-access` | `getAnonymousAccess` | `nx-admin` | M1 |
| Anonymous access | PUT | `/api/v1/anonymous-access` | `updateAnonymousAccess` | `nx-admin` | M1 |
| Anonymous access | PUT | `/api/v1/anonymous-access/repositories` | `setAnonymousRepositories` | `nx-admin` | M1 |
| Repositories | GET | `/api/v1/repositories` | `listRepositories` | `repo.browse(R) per item` | M1 |
| Repositories | POST | `/api/v1/repositories` | `createRepository` | `nx-admin` | M1 |
| Repositories | GET | `/api/v1/repositories/{name}` | `getRepository` | `repo.browse(R)` | M1 |
| Repositories | PATCH | `/api/v1/repositories/{name}` | `patchRepository` | `nx-admin` | M1 |
| Repositories | DELETE | `/api/v1/repositories/{name}` | `deleteRepository` | `nx-admin` | M1 |
| Repositories | POST | `/api/v1/repositories/{name}/actions/invalidate-cache` | `invalidateRepositoryCache` | `nx-admin` | M1 |
| Repositories | POST | `/api/v1/repositories/{name}/actions/set-blocked` | `setRepositoryBlocked` | `nx-admin` | M1 |
| Repositories | POST | `/api/v1/upstream-checks` | `checkProxyUpstream` | `nx-admin` | M1 |
| Repositories | GET | `/api/v1/repositories/{name}/health` | `getRepositoryHealth` | `repo.browse(R)` | M2 |
| Blob stores | GET | `/api/v1/blob-stores` | `listBlobStores` | `nx-admin` | M1 |
| Blob stores | POST | `/api/v1/blob-stores` | `createBlobStore` | `nx-admin` | M1 |
| Blob stores | GET | `/api/v1/blob-stores/{blob_store_name}` | `getBlobStore` | `nx-admin` | M1 |
| Blob stores | PATCH | `/api/v1/blob-stores/{blob_store_name}` | `patchBlobStore` | `nx-admin` | M1 |
| Blob stores | DELETE | `/api/v1/blob-stores/{blob_store_name}` | `deleteBlobStore` | `nx-admin` | M1 |
| Blob stores | GET | `/api/v1/blob-stores/{blob_store_name}/usage` | `getBlobStoreUsage` | `nx-admin` | M1 |
| Blob stores | POST | `/api/v1/blob-stores/{blob_store_name}/actions/test-connection` | `testBlobStoreConnection` | `nx-admin` | M1 |
| Blob stores | GET | `/api/v1/blob-stores/{blob_store_name}/members` | `listBlobStoreMembers` | `nx-admin` | M2 |
| Blob stores | PUT | `/api/v1/blob-stores/{blob_store_name}/members` | `setBlobStoreMembers` | `nx-admin` | M2 |
| Blob stores | DELETE | `/api/v1/blob-stores/{blob_store_name}/members/{member_name}` | `removeBlobStoreMember` | `nx-admin` | M2 |
| Blob stores | POST | `/api/v1/blob-stores/{blob_store_name}/actions/convert-to-group` | `convertBlobStoreToGroup` | `nx-admin` | M2 |
| Formats | GET | `/api/v1/formats` | `listFormats` | `authenticated` | M1 |
| Formats | GET | `/api/v1/formats/{format}/upload-schema` | `getUploadSchema` | `authenticated` | M1 |
| Browse | GET | `/api/v1/repositories/{name}/tree` | `getRepositoryTree` | `repo.browse(R)` | M1 |
| Components | GET | `/api/v1/components` | `listComponents` | `repo.browse(R) per item` | M1 |
| Components | GET | `/api/v1/components/{component_id}` | `getComponent` | `repo.browse(R)` | M1 |
| Components | DELETE | `/api/v1/components/{component_id}` | `deleteComponent` | `repo.delete(R)` | M1 |
| Assets | GET | `/api/v1/assets` | `listAssets` | `repo.browse(R) per item` | M1 |
| Assets | GET | `/api/v1/assets/{asset_id}` | `getAsset` | `repo.browse(R)` | M1 |
| Assets | DELETE | `/api/v1/assets/{asset_id}` | `deleteAsset` | `repo.delete(R)` | M1 |
| Search | GET | `/api/v1/search/components` | `searchComponents` | `repo.browse(R) per item` | M1 |
| Search | GET | `/api/v1/search/assets` | `searchAssets` | `repo.browse(R) per item` | M1 |
| Upload | POST | `/api/v1/components` | `uploadComponent` | `repo.write(R)` | M1 |
| Client setup | GET | `/api/v1/client-setup` | `getClientSetup` | `repo.browse(R)` | M1 |
| Cleanup policies | GET | `/api/v1/cleanup-policies` | `listCleanupPolicies` | `nx-admin` | M1 |
| Cleanup policies | POST | `/api/v1/cleanup-policies` | `createCleanupPolicy` | `nx-admin` | M1 |
| Cleanup policies | GET | `/api/v1/cleanup-policies/{policy_id}` | `getCleanupPolicy` | `nx-admin` | M1 |
| Cleanup policies | PUT | `/api/v1/cleanup-policies/{policy_id}` | `updateCleanupPolicy` | `nx-admin` | M1 |
| Cleanup policies | DELETE | `/api/v1/cleanup-policies/{policy_id}` | `deleteCleanupPolicy` | `nx-admin` | M1 |
| Cleanup policies | POST | `/api/v1/cleanup-policies/{policy_id}/actions/preview` | `previewCleanupPolicy` | `nx-admin` | M1 |
| Routing rules | GET | `/api/v1/routing-rules` | `listRoutingRules` | `nx-admin` | M1 |
| Routing rules | POST | `/api/v1/routing-rules` | `createRoutingRule` | `nx-admin` | M1 |
| Routing rules | POST | `/api/v1/routing-rules/validate` | `validateRoutingPatterns` | `nx-admin` | M1 |
| Routing rules | POST | `/api/v1/routing-rules/test` | `testDraftRoutingRule` | `nx-admin` | M1 |
| Routing rules | GET | `/api/v1/routing-rules/{rule_id}` | `getRoutingRule` | `nx-admin` | M1 |
| Routing rules | PUT | `/api/v1/routing-rules/{rule_id}` | `updateRoutingRule` | `nx-admin` | M1 |
| Routing rules | DELETE | `/api/v1/routing-rules/{rule_id}` | `deleteRoutingRule` | `nx-admin` | M1 |
| Routing rules | POST | `/api/v1/routing-rules/{rule_id}/test` | `testRoutingRule` | `nx-admin` | M1 |
| Lifecycle runs | GET | `/api/v1/lifecycle-runs` | `listLifecycleRuns` | `nx-admin` | M1 |
| Lifecycle runs | GET | `/api/v1/lifecycle-runs/{run_id}` | `getLifecycleRun` | `nx-admin` | M1 |
| Lifecycle runs | GET | `/api/v1/lifecycle-runs/{run_id}/candidates` | `listLifecycleCandidates` | `nx-admin` | M1 |
| Tasks | GET | `/api/v1/tasks` | `listTasks` | `nx-admin` | M1 |
| Tasks | POST | `/api/v1/tasks` | `createTask` | `nx-admin` | M2 |
| Tasks | GET | `/api/v1/tasks/{task_id}` | `getTask` | `nx-admin` | M1 |
| Tasks | PUT | `/api/v1/tasks/{task_id}` | `updateTask` | `nx-admin` | M2 |
| Tasks | DELETE | `/api/v1/tasks/{task_id}` | `deleteTask` | `nx-admin` | M2 |
| Tasks | GET | `/api/v1/tasks/{task_id}/runs` | `listTaskRuns` | `nx-admin` | M1 |
| Tasks | GET | `/api/v1/tasks/{task_id}/runs/{run_id}` | `getTaskRun` | `nx-admin` | M1 |
| Tasks | GET | `/api/v1/tasks/{task_id}/runs/{run_id}/log` | `getTaskRunLog` | `nx-admin` | M1 |
| Tasks | POST | `/api/v1/tasks/{task_id}/actions/run` | `runTask` | `nx-admin` | M2 |
| Tasks | POST | `/api/v1/tasks/{task_id}/runs/{run_id}/actions/stop` | `stopTaskRun` | `nx-admin` | M2 |
| Audit | GET | `/api/v1/audit-events` | `listAuditEvents` | `nx-admin` | M1 |
| Audit | GET | `/api/v1/audit-events/export` | `exportAuditEvents` | `nx-admin` | M1 |
| Audit | GET | `/api/v1/audit-events/{event_id}` | `getAuditEvent` | `nx-admin` | M1 |
| System | GET | `/api/v1/system/status` | `getSystemStatus` | `nx-admin` | M1 |
| System | GET | `/api/v1/system/info` | `getSystemInfo` | `anonymous` | M1 |
| System | GET | `/api/v1/system/metrics/summary` | `getMetricsSummary` | `nx-admin` | M1 |
| System | GET | `/api/v1/openapi.json` | `getOpenApiDocument` | `anonymous` | M1 |
| System | GET | `/api/v1/system/info/details` | `getSystemInfoDetails` | `nx-admin` | M2 |
| Dashboard | GET | `/api/v1/dashboard/summary` | `getDashboardSummary` | `anonymous` | M1 |
| Settings | GET | `/api/v1/settings/schema` | `getSettingsSchema` | `nx-admin` | M1 |
| Settings | GET | `/api/v1/settings/{section}` | `getSettingsSection` | `nx-admin` | M1 |
| Settings | PUT | `/api/v1/settings/{section}` | `updateSettingsSection` | `nx-admin` | M1 |
| Settings | POST | `/api/v1/settings/email/verify` | `verifyEmailSettings` | `nx-admin` | M2 |
| Projects | GET | `/api/v1/projects` | `listProjects` | `authenticated` | M2 |
| Projects | POST | `/api/v1/projects` | `createProject` | `authenticated` | M2 |
| Projects | GET | `/api/v1/projects/{project_id}` | `getProject` | `project.limited-guest(P) or anonymous` | M2 |
| Projects | PATCH | `/api/v1/projects/{project_id}` | `patchProject` | `project.project-admin(P) or nx-admin` | M2 |
| Projects | DELETE | `/api/v1/projects/{project_id}` | `deleteProject` | `project.project-admin(P) or nx-admin` | M2 |
| Projects | GET | `/api/v1/projects/{project_id}/repositories` | `listProjectRepositories` | `project.guest(P)` | M2 |
| Projects | POST | `/api/v1/projects/{project_id}/repositories` | `attachProjectRepository` | `project.maintainer(P) or nx-admin` | M2 |
| Projects | DELETE | `/api/v1/projects/{project_id}/repositories/{name}` | `detachProjectRepository` | `project.maintainer(P) or nx-admin` | M2 |
| Projects | GET | `/api/v1/projects/{project_id}/summary` | `getProjectSummary` | `project.limited-guest(P) or anonymous` | M2 |
| Project members | GET | `/api/v1/projects/{project_id}/members` | `listProjectMembers` | `project.limited-guest(P)` | M2 |
| Project members | POST | `/api/v1/projects/{project_id}/members` | `addProjectMember` | `project.project-admin(P) or nx-admin` | M2 |
| Project members | PUT | `/api/v1/projects/{project_id}/members/{member_id}` | `updateProjectMember` | `project.project-admin(P) or nx-admin` | M2 |
| Project members | DELETE | `/api/v1/projects/{project_id}/members/{member_id}` | `removeProjectMember` | `project.project-admin(P) or nx-admin` | M2 |
| Robot tokens | POST | `/api/v1/api-tokens` | `createRobotToken` | `project.project-admin(P) or nx-admin` | M2 |
| Robot tokens | POST | `/api/v1/api-tokens/{token_id}/actions/rotate` | `rotateApiToken` | `project.project-admin(P) or nx-admin` | M2 |
| Robot tokens | POST | `/api/v1/api-tokens/{token_id}/actions/set-enabled` | `setApiTokenEnabled` | `project.project-admin(P) or nx-admin` | M2 |
| Robot tokens | POST | `/api/v1/api-tokens/actions/purge-expired` | `purgeExpiredApiTokens` | `nx-admin` | M2 |
| Robot tokens | GET | `/api/v1/projects/{project_id}/robots` | `listProjectRobots` | `project.maintainer(P)` | M2 |
| Robot tokens | POST | `/api/v1/projects/{project_id}/robots` | `createProjectRobot` | `project.project-admin(P) or nx-admin` | M2 |
| Labels | GET | `/api/v1/labels` | `listLabels` | `authenticated` | M2 |
| Labels | POST | `/api/v1/labels` | `createLabel` | `nx-admin or project.project-admin(P)` | M2 |
| Labels | GET | `/api/v1/labels/{label_id}` | `getLabel` | `authenticated` | M2 |
| Labels | PUT | `/api/v1/labels/{label_id}` | `updateLabel` | `nx-admin or project.project-admin(P)` | M2 |
| Labels | DELETE | `/api/v1/labels/{label_id}` | `deleteLabel` | `nx-admin or project.project-admin(P)` | M2 |
| Labels | GET | `/api/v1/components/{component_id}/labels` | `listComponentLabels` | `repo.browse(R)` | M2 |
| Labels | PUT | `/api/v1/components/{component_id}/labels` | `setComponentLabels` | `repo.write(R)` | M2 |
| Retention | GET | `/api/v1/retention-policies` | `listRetentionPolicies` | `project.guest(P) or nx-admin` | M2 |
| Retention | POST | `/api/v1/retention-policies` | `createRetentionPolicy` | `project.developer(P) or nx-admin` | M2 |
| Retention | GET | `/api/v1/retention-policies/{policy_id}` | `getRetentionPolicy` | `project.guest(P) or nx-admin` | M2 |
| Retention | PUT | `/api/v1/retention-policies/{policy_id}` | `updateRetentionPolicy` | `project.developer(P) or nx-admin` | M2 |
| Retention | DELETE | `/api/v1/retention-policies/{policy_id}` | `deleteRetentionPolicy` | `project.developer(P) or nx-admin` | M2 |
| Retention | POST | `/api/v1/retention-policies/{policy_id}/actions/dry-run` | `dryRunRetentionPolicy` | `project.developer(P) or nx-admin` | M2 |
| Retention | POST | `/api/v1/retention-policies/{policy_id}/actions/run` | `runRetentionPolicy` | `project.developer(P) or nx-admin` | M2 |
| Immutability | GET | `/api/v1/immutability-rules` | `listImmutabilityRules` | `project.guest(P) or nx-admin` | M2 |
| Immutability | POST | `/api/v1/immutability-rules` | `createImmutabilityRule` | `project.maintainer(P) or nx-admin` | M2 |
| Immutability | GET | `/api/v1/immutability-rules/{rule_id}` | `getImmutabilityRule` | `project.guest(P) or nx-admin` | M2 |
| Immutability | PUT | `/api/v1/immutability-rules/{rule_id}` | `updateImmutabilityRule` | `project.maintainer(P) or nx-admin` | M2 |
| Immutability | DELETE | `/api/v1/immutability-rules/{rule_id}` | `deleteImmutabilityRule` | `project.maintainer(P) or nx-admin` | M2 |
| Immutability | POST | `/api/v1/immutability-rules/test` | `testImmutabilityRules` | `project.guest(P) or nx-admin` | M2 |
| Quotas | GET | `/api/v1/quotas` | `listQuotas` | `nx-admin` | M2 |
| Quotas | GET | `/api/v1/quotas/{quota_id}` | `getQuota` | `nx-admin` | M2 |
| Quotas | PUT | `/api/v1/quotas/{quota_id}` | `updateQuota` | `nx-admin` | M2 |
| Quotas | POST | `/api/v1/quotas/{quota_id}/actions/recount` | `recountQuota` | `nx-admin` | M2 |
| Quotas | GET | `/api/v1/projects/{project_id}/quota` | `getProjectQuota` | `project.limited-guest(P)` | M2 |
| Scanners | GET | `/api/v1/scanners` | `listScanners` | `nx-admin` | M2 |
| Scanners | POST | `/api/v1/scanners` | `createScanner` | `nx-admin` | M2 |
| Scanners | GET | `/api/v1/scanners/{scanner_id}` | `getScanner` | `nx-admin` | M2 |
| Scanners | PUT | `/api/v1/scanners/{scanner_id}` | `updateScanner` | `nx-admin` | M2 |
| Scanners | DELETE | `/api/v1/scanners/{scanner_id}` | `deleteScanner` | `nx-admin` | M2 |
| Scanners | POST | `/api/v1/scanners/{scanner_id}/actions/test` | `testScanner` | `nx-admin` | M2 |
| Scans | GET | `/api/v1/scans` | `listScans` | `nx-admin or project.maintainer(P)` | M2 |
| Scans | POST | `/api/v1/scans` | `startScans` | `repo.write(R) or project.maintainer(P)` | M2 |
| Scans | POST | `/api/v1/scans/actions/scan-all` | `scanAll` | `nx-admin` | M2 |
| Scans | POST | `/api/v1/scans/actions/stop-all` | `stopAllScans` | `nx-admin` | M2 |
| Scans | GET | `/api/v1/scans/{scan_id}` | `getScan` | `repo.browse(R)` | M2 |
| Scans | GET | `/api/v1/scans/{scan_id}/log` | `getScanLog` | `repo.browse(R)` | M2 |
| Scans | POST | `/api/v1/scans/{scan_id}/actions/stop` | `stopScan` | `repo.write(R) or project.maintainer(P)` | M2 |
| Vulnerabilities | GET | `/api/v1/components/{component_id}/vulnerabilities` | `listComponentVulnerabilities` | `repo.browse(R)` | M2 |
| Vulnerabilities | GET | `/api/v1/components/{component_id}/vulnerabilities/summary` | `getComponentVulnerabilitySummary` | `repo.browse(R)` | M2 |
| Vulnerabilities | GET | `/api/v1/vulnerabilities` | `searchVulnerabilities` | `nx-admin or project.maintainer(P)` | M2 |
| Vulnerabilities | GET | `/api/v1/vulnerabilities/overview` | `getSecurityOverview` | `nx-admin` | M2 |
| Vulnerabilities | POST | `/api/v1/vulnerabilities/exports` | `createVulnerabilityExport` | `project.maintainer(P) or nx-admin` | M2 |
| Vulnerabilities | GET | `/api/v1/vulnerabilities/exports/{export_id}` | `getVulnerabilityExport` | `project.maintainer(P) or nx-admin` | M2 |
| Vulnerabilities | GET | `/api/v1/vulnerabilities/exports/{export_id}/download` | `downloadVulnerabilityExport` | `project.maintainer(P) or nx-admin` | M2 |
| CVE allowlists | GET | `/api/v1/cve-allowlists` | `listCveAllowlist` | `project.guest(P) or nx-admin` | M2 |
| CVE allowlists | POST | `/api/v1/cve-allowlists` | `addCveAllowlistEntries` | `project.project-admin(P) or nx-admin` | M2 |
| CVE allowlists | DELETE | `/api/v1/cve-allowlists/{entry_id}` | `deleteCveAllowlistEntry` | `project.project-admin(P) or nx-admin` | M2 |
| CVE allowlists | POST | `/api/v1/cve-allowlists/actions/copy-from-system` | `copySystemCveAllowlist` | `project.project-admin(P) or nx-admin` | M2 |
| SBOM | GET | `/api/v1/components/{component_id}/sbom` | `getComponentSbom` | `repo.browse(R)` | M2 |
| SBOM | GET | `/api/v1/components/{component_id}/sbom/download` | `downloadComponentSbom` | `repo.read(R)` | M2 |
| SBOM | POST | `/api/v1/components/{component_id}/sbom/actions/generate` | `generateComponentSbom` | `repo.write(R)` | M2 |
| SBOM | GET | `/api/v1/components/{component_id}/accessories` | `listComponentAccessories` | `repo.browse(R)` | M2 |
| SBOM | DELETE | `/api/v1/components/{component_id}/accessories/{accessory_id}` | `deleteAccessory` | `repo.delete(R)` | M2 |
| Webhooks | GET | `/api/v1/webhooks` | `listWebhooks` | `project.maintainer(P) or nx-admin` | M2 |
| Webhooks | POST | `/api/v1/webhooks` | `createWebhook` | `project.project-admin(P) or nx-admin` | M2 |
| Webhooks | GET | `/api/v1/webhooks/{webhook_id}` | `getWebhook` | `project.maintainer(P) or nx-admin` | M2 |
| Webhooks | PUT | `/api/v1/webhooks/{webhook_id}` | `updateWebhook` | `project.project-admin(P) or nx-admin` | M2 |
| Webhooks | DELETE | `/api/v1/webhooks/{webhook_id}` | `deleteWebhook` | `project.project-admin(P) or nx-admin` | M2 |
| Webhooks | POST | `/api/v1/webhooks/{webhook_id}/actions/rotate-secret` | `rotateWebhookSecret` | `project.project-admin(P) or nx-admin` | M2 |
| Webhooks | GET | `/api/v1/webhook-deliveries` | `listWebhookDeliveries` | `project.maintainer(P) or nx-admin` | M2 |
| Webhooks | POST | `/api/v1/webhook-deliveries/{delivery_id}/actions/redeliver` | `redeliverWebhook` | `project.project-admin(P) or nx-admin` | M2 |
| Webhooks | POST | `/api/v1/webhooks/{webhook_id}/actions/test` | `testWebhook` | `project.project-admin(P) or nx-admin` | M2 |
| Registries | GET | `/api/v1/registries` | `listRegistries` | `nx-admin` | M2 |
| Registries | POST | `/api/v1/registries` | `createRegistry` | `nx-admin` | M2 |
| Registries | GET | `/api/v1/registries/{registry_id}` | `getRegistry` | `nx-admin` | M2 |
| Registries | PUT | `/api/v1/registries/{registry_id}` | `updateRegistry` | `nx-admin` | M2 |
| Registries | DELETE | `/api/v1/registries/{registry_id}` | `deleteRegistry` | `nx-admin` | M2 |
| Registries | POST | `/api/v1/registries/{registry_id}/actions/test` | `testRegistry` | `nx-admin` | M2 |
| Replication | GET | `/api/v1/replication-rules` | `listReplicationRules` | `nx-admin` | M2 |
| Replication | POST | `/api/v1/replication-rules` | `createReplicationRule` | `nx-admin` | M2 |
| Replication | GET | `/api/v1/replication-rules/{rule_id}` | `getReplicationRule` | `nx-admin` | M2 |
| Replication | PUT | `/api/v1/replication-rules/{rule_id}` | `updateReplicationRule` | `nx-admin` | M2 |
| Replication | DELETE | `/api/v1/replication-rules/{rule_id}` | `deleteReplicationRule` | `nx-admin` | M2 |
| Replication | POST | `/api/v1/replication-rules/{rule_id}/actions/run` | `runReplicationRule` | `nx-admin` | M2 |
| Replication | GET | `/api/v1/replication-executions` | `listReplicationExecutions` | `nx-admin` | M2 |
| Replication | GET | `/api/v1/replication-executions/{execution_id}` | `getReplicationExecution` | `nx-admin` | M2 |
| Replication | GET | `/api/v1/replication-executions/{execution_id}/tasks` | `listReplicationTasks` | `nx-admin` | M2 |
| Replication | POST | `/api/v1/replication-executions/{execution_id}/actions/stop` | `stopReplicationExecution` | `nx-admin` | M2 |
| Promotion | POST | `/api/v1/promotions` | `createPromotion` | `repo.write(R)` | M2 |
| Promotion | GET | `/api/v1/promotions/{promotion_id}` | `getPromotion` | `repo.browse(R)` | M2 |
| Trust policies | GET | `/api/v1/trust-policies` | `listTrustPolicies` | `project.guest(P) or nx-admin` | M2 |
| Trust policies | POST | `/api/v1/trust-policies` | `createTrustPolicy` | `project.project-admin(P) or nx-admin` | M2 |
| Trust policies | GET | `/api/v1/trust-policies/{policy_id}` | `getTrustPolicy` | `project.guest(P) or nx-admin` | M2 |
| Trust policies | PUT | `/api/v1/trust-policies/{policy_id}` | `updateTrustPolicy` | `project.project-admin(P) or nx-admin` | M2 |
| Trust policies | DELETE | `/api/v1/trust-policies/{policy_id}` | `deleteTrustPolicy` | `project.project-admin(P) or nx-admin` | M2 |
| Garbage collection | GET | `/api/v1/gc/runs` | `listGcRuns` | `nx-admin` | M2 |
| Garbage collection | POST | `/api/v1/gc/runs` | `startGcRun` | `nx-admin` | M2 |
| Garbage collection | GET | `/api/v1/gc/runs/{run_id}` | `getGcRun` | `nx-admin` | M2 |
| Garbage collection | POST | `/api/v1/gc/runs/{run_id}/actions/stop` | `stopGcRun` | `nx-admin` | M2 |
| Garbage collection | GET | `/api/v1/gc/runs/{run_id}/candidates` | `listGcCandidates` | `nx-admin` | M2 |
| Certificates | GET | `/api/v1/trusted-certificates` | `listTrustedCertificates` | `nx-admin` | M2 |
| Certificates | POST | `/api/v1/trusted-certificates` | `createTrustedCertificate` | `nx-admin` | M2 |
| Certificates | DELETE | `/api/v1/trusted-certificates/{certificate_id}` | `deleteTrustedCertificate` | `nx-admin` | M2 |
| Nodes | GET | `/api/v1/system/nodes` | `listNodes` | `nx-admin` | M2 |
| OCI artifacts | GET | `/api/v1/components/{component_id}/config` | `getOciConfig` | `repo.browse(R)` | M2 |
| OCI artifacts | GET | `/api/v1/components/{component_id}/children` | `listOciChildren` | `repo.browse(R)` | M2 |
| OCI artifacts | GET | `/api/v1/components/{component_id}/tags` | `listOciTags` | `repo.browse(R)` | M2 |
| OCI artifacts | PUT | `/api/v1/components/{component_id}/tags/{tag}` | `putOciTag` | `repo.write(R)` | M2 |
| OCI artifacts | DELETE | `/api/v1/components/{component_id}/tags/{tag}` | `deleteOciTag` | `repo.delete(R)` | M2 |

## 1. Scope and sources

The contract covers the native API under `/api/v1/` for everything the UI needs:

| Milestone | Content | Source |
|---|---|---|
| `UI-M1` (usable core) | sign in and session (local, LDAP, OIDC redirect flow, logout, CSRF), `/me`, repositories (hosted, proxy, group), blob stores, browse tree, components and assets, search, upload with per format form schema, client setup snippets, users, roles, privileges, content selectors (validate, test, preview), IdP group mappings, API tokens, anonymous access, cleanup policies with dry run, routing rules (lint, test), tasks (read only), audit (query, NDJSON export), system status and info, metrics summary, dashboard summary, basic settings | parity document 14.2, UI inventory sections 7 to 11 |
| `UI-M2` (registry parity, lighter schemas) | projects and members, robot tokens, labels, retention and immutability with dry run, quotas, scans and findings, scanners, CVE allowlists, SBOM and accessories, OCI tags, webhooks and deliveries, registries, replication, promotion, trust policies, GC runs, blob store groups, provider editing, task administration, settings extras, certificates, nodes, sessions | parity document sections 11 and 13 |

Out of the contract: the compat v1 compatibility API (`/service/rest/v1`), the package protocols (`/repository/...`, `/v2/...`), `/healthz`, `/readyz`, `/metrics` (owned by `rustryx-api`, see [server.md](server.md)), and UI-M3 pages.

Every operation in the YAML carries `x-milestone`, `x-required-privilege`, optionally `x-future-privilege` and, for state changes, `x-audit` (kind and action). UI-M2 operations are the ones with `x-milestone: UI-M2`; their tags are listed under the group "UI-M2 registry parity" in `x-tagGroups`.

## 2. URL and naming rules

| Rule | Detail |
|---|---|
| Base path | `/api/v1`, same origin as the UI. The YAML paths include the prefix and the server entry is `/`. TLS terminates in front of the node; HSTS is sent. |
| Collections | plural kebab-case nouns: `/repositories`, `/blob-stores`, `/idp-group-mappings`, `/api-tokens`, `/content-selectors`. |
| Identifiers | A UUID in the path (`/users/{user_id}`, UUIDv7, time ordered), except repositories and blob stores, which are addressed by their unique **name** (`/repositories/{name}`, `/blob-stores/{blob_store_name}`): the name is the identity people use, it is validated like `RepoName`, and it never changes (no rename). References inside bodies follow the core field names: `blob_store_id`, `cleanup_policy_ids`, `routing_rule_id`, `group.members` (names). |
| Sub-resources | A noun below its owner: `/repositories/{name}/tree`, `/projects/{project_id}/members`, `/tasks/{task_id}/runs/{run_id}/log`. |
| Actions | A state change that is not CRUD is `POST /{collection}/{id}/actions/{verb}` (`revoke`, `rotate`, `run`, `stop`, `reset-password`, ...), or `POST /{collection}/actions/{verb}` for the collection. Verbs are kebab-case. A synchronous action returns the changed object, a long running one returns 202 (section 14). |
| Pure computations | `validate`, `test`, `preview`, `language` helpers (content selectors, routing rules, group mappings) use POST because they take a body, change nothing, are not audited and always answer 200 with the verdict in the body. |
| Query parameters | snake_case. Equality filters use the field name (`format=maven2`), a prefix or contains search is `q`, ranges are `from` and `to` (instants, `from` inclusive and `to` exclusive), booleans are `true` and `false`, multiple values are comma separated (`kind=login_success,login_failure`, OpenAPI `style: form, explode: false`). Unknown parameters are rejected with 400 `invalid_query`. |
| JSON fields | snake_case, exactly the serde names of the rustryx-core types where an entity exists (`repo_type`, `blob_store_id`, `write_policy`, `allow_anonymous`). Enum values are lowercase words; where core uses `snake_case` so does the API (`allow_once`, `login_success`). The only hyphenated enum values are the `PrivilegeGrant.type` values core serializes that way (`repository-view`, `repository-content-selector`) and the project roles (`limited-guest`, `project-admin`). New enum values may appear, clients tolerate unknown ones. |
| Time | RFC 3339 in UTC with `Z` (`2026-10-24T09:15:30Z`), schema `format: date-time`. Durations are integer seconds with the suffix `_secs`, ages in days `_days`. |
| Sizes | integer bytes, suffix `_bytes`, schema `int64`; values stay below 2^53, so JavaScript numbers are safe. |
| Null and absent | A nullable response field is always present, with `null` for "none". A request field that is optional is omitted. In a merge patch `null` clears the field. Response objects never omit a field to mean "no". |
| Unknown request fields | Rejected with 400 `invalid_json` (request schemas are closed). Response schemas are open: clients ignore fields they do not know. |
| Secrets | Never in a response. Write only inputs are marked `writeOnly`. A token or a webhook secret is returned once, by the create operation and by rotation (`ApiTokenCreated`, `WebhookCreated`); the response is `Cache-Control: no-store`. CSRF tokens are per session values, not credentials. |

## 3. Versioning and compatibility

- The major version is in the path. `/api/v1` changes only additively: new endpoints, new optional request fields, new response fields, new enum values, new error codes. Clients ignore unknown fields, tolerate unknown enum values and treat an unknown error `code` by its HTTP status class.
- A breaking change (removal, rename, type change, new required request field, tighter validation of existing input) needs `/api/v2`, served next to v1 for at least one minor release cycle. A deprecated operation is marked `deprecated: true` in the YAML and answers with `Deprecation` and `Sunset` headers.
- `info.version` of the YAML is the contract version (`1.0.0-draft.1` until UI-M1 is accepted). The node serves the document that matches its binary at `GET /api/v1/openapi.json`; the UI client is generated from the YAML in the repository and CI fails when the generated client changes without a contract change.
- UI-M2 schemas are lighter on purpose and may gain fields before the wave starts; names, URLs and the rules in this document are stable.
- compat v1 compatibility endpoints are a separate surface with their own conventions (`continuationToken`, compat field names) and are never mixed into `/api/v1`.

## 4. Media types, bodies and limits

| Media type | Where |
|---|---|
| `application/json` | every request and response body unless stated. Requests without it get 415 `unsupported_media_type`. |
| `application/merge-patch+json` | PATCH of repositories, blob stores, users, projects (RFC 7396: objects merge, `null` clears, arrays replace as a whole). PATCH is used where an object holds write only secrets that are never echoed; everything else is replaced with PUT. |
| `multipart/form-data` | `POST /components` (a JSON `attributes` part and one or more `file` parts, streamed). |
| `application/x-ndjson` | audit export (section 15). |
| `text/csv` | lifecycle candidate list, vulnerability export (section 15). |
| `application/spdx+json` | SBOM download (UI-M2). |

Limits (from `limits.*`, see [config.md](config.md)): JSON bodies up to `limits.max_body_mb` (10 MiB, 413 `payload_too_large`), uploads up to `limits.max_blob_bytes` (5 GiB) with a body idle timeout of 60 seconds (408 `request_body_timeout`) and the request timeout (504 `request_timeout`). Responses of the API group are gzip compressed when the client asks, `Cache-Control: no-store` unless the handler says otherwise, and carry `X-Request-Id`. Request compression is not accepted.

## 5. Errors

### 5.1 Envelope

Every error response, from every layer (router, authentication, extractors, handlers), has one shape, as implemented in `rustryx-api` (`error.rs`):

```json
{ "error": { "code": "repository_not_found", "message": "Repository not found", "request_id": "0192f6c4-5a1e-7000-8000-00000000c0de" } }
```

- `code` is stable and machine readable (`^[a-z][a-z0-9_]*$`). Clients switch on `code`, never on `message`.
- `message` is written per code, safe to show, at most 240 characters; it never repeats backend error text (no SQL, paths, upstream addresses, LDAP details). The detail goes to the server log under the `request_id`.
- `request_id` equals the `X-Request-Id` response header. Clients may send their own `X-Request-Id` (1 to 64 characters of `[A-Za-z0-9._-]`); it is echoed. The UI shows `code` and `request_id` with copy buttons on every failure.
- `details` is an optional, additive extension (not implemented today, see section 19): an object that may hold `fields` (field level errors), `dependents` (objects blocking a change), `compile_errors`, `policy`, `rule`, `min_prefix`, `max_days`, `component_count`, `limit_bytes`, `used_bytes`. Clients ignore unknown keys. It never holds secrets.
- Field errors: `details.fields[]` is `{field, code, message}` where `field` is a JSON pointer into the request body (`/proxy/remote_url`) or the query parameter name. The UI maps them to form fields and focuses the first.
- Status class rules: 4xx the caller can fix, 5xx the server or a dependency; 429 and 503 always carry `Retry-After` (whole seconds, at least 1). 401 carries `WWW-Authenticate`.
- Domain validation is **400 `invalid_input`**, not 422 (the UI inventory says 422; `DomainError::Invalid` maps to 400 in `rustryx-api`, and the contract follows the implementation). 422 is not used by the native API.

### 5.2 Error code catalogue

`origin` says where the code lives: `api` is implemented in `rustryx-api` today, `format-core` is used by the format engine, `contract` is new in this contract and must be added to `rustryx-api` (section 19). The same list is in the YAML as `x-known-codes` of `ErrorBody`.

| `code` | Status | Origin | Meaning |
|---|---|---|---|
| `digest_mismatch` | 400 | format-core | A declared checksum does not match the received bytes. |
| `immutable_field` | 400 | contract | The request tries to change a field that cannot change after creation (`details.fields`). |
| `invalid_body` | 400 | api | The request body could not be read. |
| `invalid_input` | 400 | api | A domain rule rejected the input. `details.fields` lists the offending fields when known. |
| `invalid_json` | 400 | api | The body is not valid JSON or does not match the request schema (unknown field, wrong type). |
| `invalid_page_token` | 400 | contract | `after` is malformed or was issued for another endpoint or sort order. Start again from the first page. |
| `invalid_path` | 400 | api | A path parameter is malformed (for example a bad UUID). |
| `invalid_pattern` | 400 | contract | A routing rule or retention pattern is rejected by the linear-time regex engine (`details.fields`). |
| `invalid_query` | 400 | api | A query parameter is malformed, out of range, or unknown. |
| `invalid_selector` | 400 | contract | A content selector expression does not compile (`details.compile_errors`). |
| `oidc_callback_invalid` | 400 | contract | The OIDC callback cookie, state or ID token is invalid or expired. |
| `query_too_short` | 400 | contract | A search term is shorter than the minimum prefix (`details.min_prefix`). |
| `range_too_large` | 400 | contract | A time range exceeds the maximum the endpoint serves (`details.max_days`). |
| `unsupported_sort` | 400 | contract | `sort` names a field the endpoint cannot order by. |
| `upstream_url_rejected` | 400 | contract | A remote, registry or webhook URL was refused by the SSRF filter (`details.rule` names the rule). |
| `invalid_credentials` | 401 | api | Wrong user or password, disabled account, or the identity provider refused the code (deliberately not more specific). |
| `invalid_token` | 401 | api | A bearer token is malformed, badly signed, or no authenticator claims it. |
| `token_expired` | 401 | api | A bearer token or the cookie session is past its expiry (idle or absolute). |
| `token_revoked` | 401 | api | The API token or session was revoked. |
| `unauthenticated` | 401 | api | Credentials are required. Also the answer for an anonymous caller that is denied. |
| `csrf_rejected` | 403 | contract | A cookie-authenticated unsafe request has a missing or wrong CSRF token, or a foreign `Origin`. |
| `forbidden` | 403 | api | Authenticated but not allowed (also the fail-closed answer for a failed group lookup). |
| `api_token_not_found` | 404 | api | The token does not exist or belongs to someone else. |
| `asset_not_found` | 404 | api | The asset does not exist or its repository is hidden from the caller. |
| `audit_event_not_found` | 404 | api | The audit event does not exist or was dropped by retention. |
| `blob_not_found` | 404 | api | The object behind an asset is missing in the blob store (integrity problem). |
| `blob_store_not_found` | 404 | api | The blob store does not exist. |
| `cleanup_policy_not_found` | 404 | api | The cleanup policy does not exist. |
| `component_not_found` | 404 | api | The component does not exist or its repository is hidden from the caller. |
| `content_selector_not_found` | 404 | api | The content selector does not exist. |
| `format_not_found` | 404 | api | The format label is unknown. |
| `idp_group_mapping_not_found` | 404 | api | The group mapping does not exist. |
| `lifecycle_run_not_found` | 404 | api | The lifecycle run does not exist. |
| `not_found` | 404 | api | The resource does not exist (store level). |
| `privilege_not_found` | 404 | api | The privilege does not exist. |
| `repository_not_found` | 404 | api | The repository does not exist or is hidden from the caller. |
| `role_not_found` | 404 | api | The role does not exist. |
| `route_not_found` | 404 | api | No such route. |
| `routing_rule_not_found` | 404 | api | The routing rule does not exist. |
| `task_not_found` | 404 | api | The task does not exist. |
| `task_run_not_found` | 404 | api | The task run does not exist. |
| `user_not_found` | 404 | api | The user does not exist. |
| `<entity>_not_found` | 404 | api | Pattern for every other entity: `DomainError::NotFound` renders `<entity>_not_found` (UI-M2 entities follow it, for example `project_not_found`). |
| `method_not_allowed` | 405 | api | The path exists but not for this method (`Allow` header). |
| `request_body_timeout` | 408 | api | The client stopped sending the body. |
| `already_exists` | 409 | api | An entity with that natural key (name) exists. |
| `builtin_read_only` | 409 | contract | Built-in roles and privileges cannot be edited or deleted. |
| `cannot_modify_self` | 409 | contract | An administrator cannot disable, delete or demote the account in use. |
| `conflict` | 409 | api | The request conflicts with the current state (generic). |
| `external_identity` | 409 | contract | The field belongs to the identity provider (password, e-mail, display name of LDAP and OIDC accounts). |
| `idempotency_conflict` | 409 | contract | The `Idempotency-Key` was used with a different request. |
| `immutable_tag` | 409 | contract | An immutability rule protects the item (`details.rule`). UI-M2. |
| `last_admin` | 409 | contract | The change would remove the last local `nx-admin`. |
| `not_ready` | 409 | contract | The target of the call is not in a state that allows it yet (export not finished, scan still running). UI-M2. |
| `repository_not_empty` | 409 | contract | Deleting needs `include_content=true` because the repository still holds components (`details.component_count`). |
| `repository_not_hosted` | 409 | contract | Upload, delete or write needs a hosted repository. |
| `resource_in_use` | 409 | contract | The object is referenced by others (`details.dependents`). Nothing was changed. |
| `role_cycle` | 409 | contract | The nesting of roles would form a cycle. |
| `run_in_progress` | 409 | contract | A run of this kind is already active (one active run per task, rule or store). |
| `setting_managed_by_config` | 409 | contract | The value comes from the configuration file or environment and cannot change at runtime. |
| `unsupported_for_format` | 409 | contract | The operation does not apply to the repository format (for example a path tree of an OCI repository). |
| `username_conflict` | 409 | api | The username is used by another identity source; accounts are never merged automatically. |
| `write_policy_violation` | 409 | contract | The repository write policy forbids the write (`details.policy` is `allow_once` or `deny`). |
| `precondition_failed` | 412 | contract | `If-Match` does not match the current version: someone else changed the object. |
| `payload_too_large` | 413 | api | The body is larger than the route limit (`limits.max_body_mb`, uploads `limits.max_blob_bytes`). |
| `unsupported_media_type` | 415 | api | The content type is not accepted by the endpoint. |
| `range_not_satisfiable` | 416 | api | Reserved for download style endpoints. |
| `precondition_required` | 428 | contract | The endpoint needs `If-Match`. |
| `locked_out` | 429 | api | Too many failed logins for the account or the address (`Retry-After`). |
| `rate_limited` | 429 | api | Per-client token bucket exhausted (`Retry-After`). |
| `internal_error` | 500 | api | Unexpected failure. The detail is in the server log under the `request_id`. |
| `upstream_error` | 502 | api | An upstream repository answered with something unusable. |
| `authentication_unavailable` | 503 | api | Credentials were presented but no authenticator is wired. |
| `authorization_unavailable` | 503 | api | The authorization checker is not wired or failed. |
| `blob_store_unavailable` | 503 | api | A blob store is unreachable (`Retry-After`). |
| `dependency_unavailable` | 503 | api | Another dependency is down (`Retry-After`). |
| `identity_provider_unavailable` | 503 | api | Keycloak or LDAP is unreachable. New interactive SSO logins fail, local users and `ryx_*` tokens keep working. |
| `login_unavailable` | 503 | api | The failed-login counter store is down; password logins fail closed. |
| `overloaded` | 503 | api | The node shed the request (`Retry-After`). |
| `read_only_mode` | 503 | contract | The system is in read-only mode (`Retry-After`): reads work, writes are refused. UI-M2 setting. |
| `store_unavailable` | 503 | api | PostgreSQL is unreachable or timed out (`Retry-After`). |
| `request_timeout` | 504 | api | Producing the response took longer than `limits.request_timeout_secs`. |
| `upstream_timeout` | 504 | api | An upstream repository did not answer in time. |
| `quota_exceeded` | 507 | api | A repository, blob store or project quota would be exceeded (`details.limit_bytes`, `details.used_bytes`). |

### 5.3 What a client does with an error

| Situation | Client behaviour |
|---|---|
| 401 on a cookie session (`unauthenticated`, `token_expired`, `token_revoked`) | show the session expired dialog, sign in again, return to the same URL. |
| 401 `invalid_credentials` on the login form | show one generic message (no user enumeration). |
| 403 `csrf_rejected` | `GET /api/v1/auth/csrf` once, repeat the request once; if it fails again reload the page. |
| 403 `forbidden` | show "You do not have access" with the privilege from `x-required-privilege`; actions are hidden, not disabled, when `/me` already says so. |
| 404 | "Not found or hidden from you": the API answers 404 for repositories the caller may not browse. |
| 409 | show `message` and, when present, `details.dependents` as links. Never retry automatically. |
| 412 | three way dialog: reload, overwrite (repeat with the fresh `ETag`), copy my changes. |
| 428 | a client bug: the operation needs `If-Match`. |
| 429, 503 | countdown from `Retry-After`, then retry reads automatically. Unsafe requests are not retried automatically unless they carry `Idempotency-Key`. Keep showing the last data. |
| 5xx | show `code` and `request_id`. |
| unknown `code` | handle by status class. |

## 6. Pagination

Keyset pagination everywhere, matching `Page<T>` of rustryx-core.

- Request: `limit` (integer, default 100, at most 1000; heavy endpoints state their own default and maximum: search and audit default 50 and at most 200, tree default 200 and at most 500) and `after` (opaque token). A `limit` above the maximum is clamped, below 1 is 400 `invalid_query`.
- Response: `{ "items": [...], "next": "<token>" | null }`. `next` is always present; `null` marks the last page. Pass `next` back as `after`. Some pages add context fields next to `items` and `next` (`TreePage.path`, `SearchComponentPage.truncated`).
- The token is opaque (clients never parse or build one), URL safe, and carries a position, not a permission: filters are applied on every request. It is valid only with the same endpoint and sort order; start again from the first page when a filter or the sort changes. A malformed token, or one issued for another endpoint or sort, is 400 `invalid_page_token`.
- A page can hold fewer items than `limit` even when `next` is not null: items the caller may not see (permissions, content selectors) are dropped after the fetch. Only `next: null` ends a list.
- No total counts: keyset pagination has no cheap total. Counts that matter are explicit fields (`RepositoryStats`, dashboard counters).
- Items created or deleted while a client pages are neither duplicated nor reordered; an item changed to sort elsewhere may be missed or seen once.
- Bounded collections (roles, privileges, selectors, blob stores, cleanup policies, routing rules, tasks, formats) return the whole set in one page with `next: null`; `limit` and `after` are accepted so one client component serves every list.
- Search results are limited to the first 1000 hits of a query (`truncated: true`).

## 7. Filtering and sorting

- Filters are query parameters, documented per operation, combined with AND. Equality filters take the exact value; `q` is a prefix or contains search stated per operation; multi-valued filters are comma separated.
- `sort` takes one field, `-` prefix for descending, from a closed list per operation (the default is the first value or the stated default). Any other value is 400 `unsupported_sort`. The page token is bound to the sort order.
- Default orders: repositories and tasks by name, users by username, components by newest first, assets by path in byte order, audit events newest first, runs newest first.
- Filtering by a text value never reveals hidden objects: filters run inside the permission filter.
- A search needs at least one criterion. A trailing `*` is a prefix match and needs at least 3 characters before it (`query_too_short`, `details.min_prefix`); quoted phrases match in order. Only checksum lookups are exact.

## 8. Concurrent edits: ETag and If-Match

Mutable administration objects are protected against lost updates (UI inventory principle 8).

- **Reads.** `GET` of a single object (and the object returned by create, PUT and PATCH) carries a strong `ETag` (quoted, opaque). Lists do not.
- **What the ETag covers.** The writable state of the object (for a role: name, description, privilege ids, nested role ids), never usage counters, statistics or timestamps that change without an edit. Core entities have no version column except `updated_at`, so the server derives the ETag from the stored row (a version counter or a hash of the canonical writable representation) and checks it inside the write transaction (`SELECT ... FOR UPDATE`), so two writers cannot both pass.
- **Writes.** `PUT` and `PATCH` of an object **require** `If-Match` with the ETag of the version being edited: missing is 428 `precondition_required`, not matching is 412 `precondition_failed`; `If-Match: *` is not supported and counts as a mismatch. `DELETE` accepts `If-Match` optionally and enforces it when present. The new ETag is in the response.
- **409 versus 412.** 412 says "someone changed this object since you read it": reload and merge. 409 says "the request conflicts with the state whatever the version": name taken (`already_exists`), still in use (`resource_in_use`), nesting cycle (`role_cycle`), write policy (`write_policy_violation`), a run already active (`run_in_progress`). Nothing was changed in either case.
- **Exceptions (no If-Match).** `PUT /me/password` (own credential), `PUT /anonymous-access/repositories` (bulk idempotent set), `PUT /components/{id}/labels` (replaces a set), `PUT /components/{id}/tags/{tag}` (moves a pointer). Actions (`POST .../actions/...`) are state transitions and are idempotent or guarded by 409.
- Conditional GET (`If-None-Match`, 304) is allowed for a handler to support but is not promised; the API group sends `Cache-Control: no-store`.

## 9. Idempotency and retries

| Method | Rule |
|---|---|
| GET, HEAD | safe, retry freely. |
| PUT | replaces; repeating the same request with the same `If-Match` gives the same state (a repeat after success is 412 because the ETag moved: re-read). |
| PATCH | merge patch is idempotent for the same body; guarded by `If-Match` like PUT. |
| DELETE | a repeat answers 404 (`<entity>_not_found`); clients treat 404 on a retried DELETE as success. |
| POST create by natural key (name) | a retry after success is 409 `already_exists`: look the object up before retrying. |
| POST that starts a run or uploads (`Idempotency-Key` supported) | send `Idempotency-Key` (8 to 128 characters of `[A-Za-z0-9._:-]`). A retry within 24 hours with the same key and an equal body returns the first response with `Idempotent-Replayed: true`; the same key with another body is 409 `idempotency_conflict`. The key is scoped to the principal and the route. |
| POST create token or webhook (secret in the response) | **not replayable**: no `Idempotency-Key`, because replaying would mean storing the secret. A retried request creates a second object; list and revoke duplicates. |
| POST actions that toggle (`revoke`, `stop`, `set-enabled`) | idempotent: repeating answers 200 with the current state. |
| POST login, logout, refresh | login is not idempotent (it counts attempts), logout and refresh are. |

Upload semantics: under write policy `allow` a retry of identical bytes overwrites with the same content; under `allow_once` a retry answers 409 `write_policy_violation` unless the first response was replayed through `Idempotency-Key`. A client that disconnects mid upload leaves nothing behind (the staged blob is aborted).

## 10. Rate limits and overload

| Condition | Answer |
|---|---|
| Per client token bucket empty (default 50 requests per second, burst 100, keyed by the authenticated user, by the client address when anonymous) | 429 `rate_limited`, `Retry-After`. |
| Failed login counter locked (5 failures, then 1 minute doubling to 30 minutes, per account and per account plus address) | 429 `locked_out`, `Retry-After`. |
| Node concurrency limit reached (`limits.max_in_flight_requests`) or no blocking permit in time | 503 `overloaded`, `Retry-After`. |
| PostgreSQL, a blob store or another dependency down | 503 `store_unavailable`, `blob_store_unavailable`, `dependency_unavailable`, `Retry-After`. |
| Identity provider down | 503 `identity_provider_unavailable` for new SSO logins only. |
| System in read-only mode (UI-M2 setting) | 503 `read_only_mode` on writes, reads keep working. |

`Retry-After` is an integer number of seconds (never an HTTP date). The rate limiter and the load shedder run before the handler, so a 429 or 503 means the operation did not start. Clients back off with jitter and honour `Retry-After`; reads retry automatically, writes only with an `Idempotency-Key`. A manual GC run is additionally limited to one per minute (429 `rate_limited`). No `RateLimit-*` headers are sent.

## 11. Authentication, sessions and CSRF

### 11.1 Credentials

| Scheme (YAML) | Credential | Used by |
|---|---|---|
| `sessionCookie` + `csrfToken` | cookie `__Host-rustryx_session` (internal JWT) plus header `X-CSRF-Token` on POST, PUT, PATCH, DELETE | the web UI |
| `apiToken` | `Authorization: Bearer ryx_...` (or HTTP Basic with the token as password, for tools that cannot send Bearer) | automation, CI, CLI |
| `keycloakToken` | `Authorization: Bearer <Keycloak access token>` | CLI and CI with SSO |

Rules enforced by the authentication middleware of `rustryx-api` and kept by the contract:

- Credentials come only from the `Authorization` header and the session cookie, never from the query string. A header that is present but unusable (unknown scheme, malformed, duplicated) is 401, never anonymous.
- When an `Authorization` header is present the cookie is ignored, so cookie and Basic or Bearer are never mixed on one request, and the CSRF check applies only when the cookie is the credential in use.
- No header and no cookie is anonymous. Anonymous callers are evaluated as `nx-anonymous`, subject to the two anonymous switches (section 11.7).
- Interactive operations (the ones whose `security` list has no `apiToken` entry: create or rotate a token, change the password, refresh the session, list sessions; `x-required-privilege: interactive` or a role term for robot tokens) accept the cookie session and Keycloak or internal JWT bearers only. A `ryx_*` token gets 403 `forbidden`: a leaked token cannot mint further tokens or change the password.
- ID tokens, offline tokens and `alg=none` are refused. Password Basic against `/api/v1` is not part of the contract (it works through the shared middleware for tools, subject to the login guard); the UI never uses it.

### 11.2 Cookie session and the internal JWT

| Item | Rule |
|---|---|
| Cookie | `__Host-rustryx_session`: `HttpOnly; Secure; SameSite=Lax; Path=/`, no `Domain` (the `__Host-` prefix forces this). `Lax` because the OIDC callback is a cross-site top level navigation. Over plain HTTP the prefix does not work: use HTTPS or `http://localhost` (browsers accept `Secure` cookies there) in development. |
| Content | the internal JWT: EdDSA, mandatory `kid`, claims `iss`, `sub` (user id), `tid` (session id), `iat`, `exp`, and for SSO users the group snapshot `grp` and `gat` ([auth-core.md](auth-core.md) ADR-2). Nothing else is stored in the browser: no token in `localStorage`, none in the DOM. |
| Idle timeout | 30 minutes (`auth.session.idle_timeout_secs`, at most 1800), sliding. |
| Absolute lifetime | 8 hours from login (`auth.session.absolute_timeout_secs`, at most 28800). Neither activity nor the Keycloak SSO session (10 hours) extends it. `exp - iat` never exceeds 8 hours. |
| Refresh of the JWT | The server reissues the cookie transparently (a `Set-Cookie` on a normal response) when less than half of the idle time is left or when the SSO group snapshot is older than 2 minutes (the chain refuses a snapshot older than 5 minutes). The SPA also calls `POST /api/v1/auth/session/refresh` from a keep-alive timer while the user is active; the response is the new `Session`. `absolute_expires_at` never moves. A reissue revokes the token it replaced after a grace of 30 seconds (`SessionSettings::revoke_grace`, at most 5 minutes), so that requests already in flight with the old cookie are not refused and a copy of the old cookie stops working shortly after. `POST /api/v1/auth/logout` ends the current token at once and any predecessor still waiting out its grace. |
| End | idle or absolute expiry, logout, `tokens_valid_after` moved (sign out everywhere, password change, user disabled), `tid` revoked: the next request is 401 `token_expired` or `token_revoked`. For SSO sessions a failed group refresh (user removed from the group, refresh token refused) ends the session the same way. |
| State of the session | `GET /api/v1/auth/session` (never extends it): `authenticated`, user, `idle_expires_at`, `absolute_expires_at`, `csrf_token`. The SPA shows a countdown before expiry and offers to refresh. |

### 11.3 CSRF and Origin

SEC-36: cookie sessions need a CSRF defence on every unsafe method.

- **Token.** A signed double submit value bound to the session (HMAC over `tid` with the server key). It is set as the cookie `__Host-rustryx_csrf` (`Secure; SameSite=Strict; Path=/`, readable by script) and returned as `csrf_token` of `Session` and `CsrfToken`. The SPA sends it as `X-CSRF-Token`.
- **Check.** On POST, PUT, PATCH and DELETE authenticated by the cookie the server requires the header to equal the cookie, the signature to verify against the session, and the `Origin` header to be the service origin (`server.base_url`, or an entry of `server.cors_allowed_origins`). A missing `Origin` on such a request is refused. Failure is 403 `csrf_rejected` and nothing is executed.
- **Login.** `POST /auth/login` has no session yet, so it carries no CSRF token; the `Origin` check is the login CSRF defence, and the cookies it sets are `SameSite`.
- **Not subject.** Requests authenticated by a bearer credential, and safe methods (GET, HEAD). Bearer clients must not send the cookie or the CSRF header.
- **Recovery.** After `csrf_rejected` call `GET /api/v1/auth/csrf` and repeat once.

### 11.4 OIDC code flow with a cookie session

Keycloak authorization code with PKCE S256, confidential client `rustryx-ui`, redirect URI `<server.base_url>/api/v1/auth/oidc/callback` ([oidc.md](oidc.md), README 8.3).

1. The SPA offers the SSO button only when `GET /auth/providers` says `oidc.enabled` and `oidc.available`. It sends the browser (top level navigation, not XHR) to `GET /api/v1/auth/oidc/login?return_to=/repositories`.
2. The node creates `state`, `nonce` and the PKCE verifier, seals them together with the validated `return_to` (a plain absolute path, never a URL: open redirect protection) in a short lived cookie (AES-256-GCM with `auth.cookie_key`, `HttpOnly; Secure; SameSite=Lax`, `Path` = the callback path, at most 10 minutes) and answers 302 to the Keycloak authorization endpoint. Any node can serve the callback later because the state lives in the cookie. Keycloak unreachable: 503 `identity_provider_unavailable` and nothing else is affected.
3. Keycloak authenticates the user and redirects to the callback with `code` and `state`.
4. The callback validates the cookie (single use, not expired), `state`, exchanges the code with the verifier, validates the ID token (issuer, audience, nonce, signature RS256 or ES256) and reads `groups` (full group paths) from the token. Roles come from the admin groups, `idp_group_mappings` and the default role; no role and no default role is 403 `forbidden`. The account is found or created by `(iss, sub)` (JIT per `auth.jit_mode`); a username used by another source is 409 `username_conflict`.
5. The node issues the internal JWT into `__Host-rustryx_session`, sets `__Host-rustryx_csrf`, clears the state cookie and answers 302 to `return_to`.
6. The SPA calls `GET /auth/session` and `GET /me`.

Callback failures: for a browser (`Accept` prefers `text/html`) every failure is a 302 to `/login?error=<code>&request_id=<id>` so the user sees the login page and not JSON; other callers get the envelope. Mapping: bad cookie, state or ID token is 400 `oidc_callback_invalid`; Keycloak refused the code 401 `invalid_credentials`; Keycloak down 503 `identity_provider_unavailable`; no role 403 `forbidden`; name clash 409 `username_conflict`. Offline tokens and `offline_access` are never requested.

Logout: `POST /api/v1/auth/logout` revokes `tid`, clears both cookies and returns `redirect_url` (the Keycloak end session URL when RP-initiated logout is configured, else null). Back-channel logout is not part of phase 1.

### 11.5 Local and LDAP login

`POST /api/v1/auth/login` with `{username, password}`: the account source decides (local argon2id, or an LDAP bind). Wrong password, unknown user and disabled account are all 401 `invalid_credentials`. Failures are counted in PostgreSQL per account and per account plus address (5 failures lock for 1 minute, doubling to 30 minutes): 429 `locked_out` with `Retry-After`. The counter store down means password logins fail closed: 503 `login_unavailable`. An unknown account still costs one dummy argon2id verification. The local break-glass administrator signs in here and works without Keycloak; so do local users and `ryx_*` tokens while SSO is down.

### 11.6 Tokens and Keycloak bearer

- API tokens: `ryx_` plus 43 characters, only the SHA-256 is stored, created with a mandatory lifetime (90 days default, 365 at most; a robot token without expiry needs `nx-admin` and a setting, UI-M2). Scopes (`*` or `repo:<repository|*>:<browse|read|write|delete|*>`) only narrow the owner's rights: the effective right is the intersection; a malformed scope is a 400 at creation, never ignored; a scope restricted token is never an administrator. Tokens are never cached, so revocation applies on the next request on every node.
- Keycloak access tokens are validated against the JWKS (`iss`, `aud` contains the product client id, `azp`, RS256 or ES256, 60 seconds clock skew). Roles come from the `groups` claim of that token on every request.

### 11.7 Anonymous access

Anonymous read needs both `auth.anonymous_enabled` (global) and `allow_anonymous` on the repository (`GET /anonymous-access`, `PUT /anonymous-access/repositories`); anonymous write and delete are never allowed. Anonymous callers of operations that allow it get the same 404 for hidden repositories as everyone else. In UI-M1 the global switch is a configuration value (`global_editable: false`).

### 11.8 CORS and headers

The SPA is served from the same origin as the API, so the API sends no CORS headers (`server.cors_allowed_origins` stays empty) and cookies are never sent cross-origin. If a development server on another origin is configured, the CORS layer of `rustryx-api` must additionally allow the request headers `If-Match`, `Idempotency-Key` and `X-CSRF-Token`, expose `ETag`, `Location` and `Retry-After`, and allow credentials for that exact origin (today it allows none). Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Strict-Transport-Security`, `X-Frame-Options: DENY` and a restrictive `Content-Security-Policy`; the static SPA sets its own CSP (no inline script).

## 12. Authorization

One authorization engine (`rustryx-rbac`): roles hold privileges, privileges only allow, there are no deny rules ([rbac.md](rbac.md)).

`x-required-privilege` grammar (checked by `check.sh`):

| Term | Meaning |
|---|---|
| `anonymous` | no login needed. Operations that read content still apply the two anonymous switches and answer 404 for hidden repositories. |
| `authenticated` | any authenticated principal. |
| `interactive` | authenticated by cookie session or Keycloak or internal JWT bearer; `ryx_*` tokens are refused with 403. Token minting by a project role (robot tokens) has the role term in `x-required-privilege` and the same credential rule in its `security` list. |
| `nx-admin` | the principal holds the built-in role `nx-admin` (matched by role id, not by name) and its token, if any, is unrestricted. |
| `repo.browse(R)`, `repo.read(R)`, `repo.write(R)`, `repo.delete(R)` | the action on repository R, taken from the path, query or body, through privileges of kind repository-view or repository-content-selector. `browse` is UI and search, `read` is client download. With ` per item` the list is filtered: items the caller may not browse are omitted. |
| `repo.admin(R)`, `repo.admin(*)` | configure repository R (or create repositories). Phase 1: only `nx-admin`. |
| `project.<role>(P)` | a project role on project P: `limited-guest`, `guest`, `developer`, `maintainer`, `project-admin` (UI-M2). |
| `A or B` | either term. |

Notes:

- **Phase 1 gate.** Every administration operation in UI-M1 requires `nx-admin`. The extension `x-future-privilege` records the per area application privilege that replaces it with AUTH-46 (`app.users.read`, `app.roles.write`, ...), so handlers can switch without a contract change. Phase 1 repository administration (`repo.admin`) is also `nx-admin`.
- **404 versus 403.** A repository, component or asset the caller may not browse is 404 (`<entity>_not_found`), never 403: existence is not revealed. 403 is for objects the caller can see but may not change. An anonymous caller that is denied gets 401 so clients ask for credentials. A group lookup that failed is 403 (fail closed), a store failure in the checker is 503.
- **Selectors.** Content selectors add access inside a repository and never remove other grants. List and tree endpoints evaluate them per item (cost bounded by SEC-18), so pages can be short.
- **Token scopes** narrow every request, administrators included; a scope restricted token cannot call administration operations.
- **Propagation.** Changes to roles, privileges, selectors and mappings reach every node through `NOTIFY` and the caches have a TTL of at most 60 seconds, so a change is effective on the next request on every node and within 60 seconds in the worst case; group membership of the current request is always read from the request's own token or snapshot.
- **`permissions` of `/me`** (admin areas, repository grants, `can.*`) and `permissions` of `RepositoryView` are conveniences for hiding actions; the server decides every request on its own.
- Admin operations that touch another user's secrets (reset password, sign out a user, revoke any token) are audited with the administrator as actor.

## 13. Audit

Audit is always on, append-only, stored in monthly partitions (90 days online by default) and written as JSON to standard output ([db.md](db.md) section 10). The native API rules:

- **What is audited.** Every operation that changes state and carries `x-audit` in the YAML (table below), every login attempt (`login_success` and `login_failure`, with the attempted name for failures), token create, revoke, rotate and purge, role, privilege, selector, mapping and user changes, repository, blob store, cleanup, routing, settings and anonymous access changes, uploads and deletes of content, and the audit export itself. Reads, validate, test and preview calls are not audited. Operations without `x-audit` that use POST write nothing because they change nothing.
- **Event.** The core `AuditEvent`: `at`, `kind`, `actor_id`, `actor_name`, `client_ip` (after trusted proxy handling), `request_id`, `action` (`<resource>.<verb>`), `target` (object name or id), `outcome` (`success` or `failure`) and `detail` (JSON). The API adds the row `id`.
- **Detail never contains** passwords, tokens, secrets, request bodies or file content. It holds names of changed fields and, for non secret scalars, old and new values; deletions by retention or cleanup are logged in batches that list digests or coordinates, so volume stays bounded.
- **Kinds.** `login_success`, `login_failure` (sign in, SSO callback, logout), `rbac_change` (users, roles, privileges, selectors, mappings, project members, password changes), `token_change` (API tokens, robot tokens, sessions), `config_change` (repositories, blob stores, cleanup policies, routing rules, settings, anonymous access, tasks, scanners, registries, webhooks, replication, GC, exports, tests that touch a remote), `content_change` (upload, delete, tag, label, promotion, retention runs, scans).
- **Failures.** Refused logins are `login_failure`. A refused mutation (403, 409, validation) is not an audit event, except SSRF filter hits and write policy or immutability rejections, which are recorded as `config_change` or `content_change` with outcome `failure`. A dedicated `access_denied` kind is proposed (SEC-39 lists 403 denials) but core has none.
- **Consistency.** For `login_*`, `rbac_change`, `token_change` and `config_change` the event is written in the same database transaction as the change when the store supports it; if the audit table cannot be written the operation fails closed with 503 `store_unavailable` and nothing changes. `content_change` is written after the commit: a failing sink is logged and counted and never undoes an upload (format-core behaviour).
- **Proposed kinds** (not in core today): `logout`, `access_denied`, `task_run`, `system`. Until they exist logout is recorded as `login_success` with action `auth.logout`. Clients tolerate unknown kinds.
- **Query and export.** `GET /audit-events` (filters: kind, actor, action prefix, target, repository, outcome, request id, time range, project) and `GET /audit-events/export` (NDJSON), administrators only. Retention drops whole months; an older range is empty.

Audit actions written by the operations of this contract (generated from `x-audit` in the YAML):

| Kind | Action | Operations | Milestone |
|---|---|---|---|
| `config_change` | `anonymous_access.repositories` | `setAnonymousRepositories` | M1 |
| `config_change` | `anonymous_access.update` | `updateAnonymousAccess` | M1 |
| `config_change` | `audit.export` | `exportAuditEvents` | M1 |
| `config_change` | `auth_provider.ldap_update` | `updateLdapProvider` | M2 |
| `config_change` | `auth_provider.ldap_verify` | `verifyLdapProvider` | M2 |
| `config_change` | `auth_provider.oidc_update` | `updateOidcProvider` | M2 |
| `config_change` | `blob_store.convert_to_group` | `convertBlobStoreToGroup` | M2 |
| `config_change` | `blob_store.create` | `createBlobStore` | M1 |
| `config_change` | `blob_store.delete` | `deleteBlobStore` | M1 |
| `config_change` | `blob_store.member_remove` | `removeBlobStoreMember` | M2 |
| `config_change` | `blob_store.members_set` | `setBlobStoreMembers` | M2 |
| `config_change` | `blob_store.test` | `testBlobStoreConnection` | M1 |
| `config_change` | `blob_store.update` | `patchBlobStore` | M1 |
| `config_change` | `cleanup_policy.create` | `createCleanupPolicy` | M1 |
| `config_change` | `cleanup_policy.delete` | `deleteCleanupPolicy` | M1 |
| `config_change` | `cleanup_policy.preview` | `previewCleanupPolicy` | M1 |
| `config_change` | `cleanup_policy.update` | `updateCleanupPolicy` | M1 |
| `config_change` | `cve_allowlist.add` | `addCveAllowlistEntries` | M2 |
| `config_change` | `cve_allowlist.copy` | `copySystemCveAllowlist` | M2 |
| `config_change` | `cve_allowlist.remove` | `deleteCveAllowlistEntry` | M2 |
| `config_change` | `gc.start` | `startGcRun` | M2 |
| `config_change` | `gc.stop` | `stopGcRun` | M2 |
| `config_change` | `immutability_rule.create` | `createImmutabilityRule` | M2 |
| `config_change` | `immutability_rule.delete` | `deleteImmutabilityRule` | M2 |
| `config_change` | `immutability_rule.update` | `updateImmutabilityRule` | M2 |
| `config_change` | `label.create` | `createLabel` | M2 |
| `config_change` | `label.delete` | `deleteLabel` | M2 |
| `config_change` | `label.update` | `updateLabel` | M2 |
| `config_change` | `project.attach_repository` | `attachProjectRepository` | M2 |
| `config_change` | `project.create` | `createProject` | M2 |
| `config_change` | `project.delete` | `deleteProject` | M2 |
| `config_change` | `project.detach_repository` | `detachProjectRepository` | M2 |
| `config_change` | `project.update` | `patchProject` | M2 |
| `config_change` | `quota.recount` | `recountQuota` | M2 |
| `config_change` | `quota.update` | `updateQuota` | M2 |
| `config_change` | `registry.create` | `createRegistry` | M2 |
| `config_change` | `registry.delete` | `deleteRegistry` | M2 |
| `config_change` | `registry.test` | `testRegistry` | M2 |
| `config_change` | `registry.update` | `updateRegistry` | M2 |
| `config_change` | `replication_execution.stop` | `stopReplicationExecution` | M2 |
| `config_change` | `replication_rule.create` | `createReplicationRule` | M2 |
| `config_change` | `replication_rule.delete` | `deleteReplicationRule` | M2 |
| `config_change` | `replication_rule.run` | `runReplicationRule` | M2 |
| `config_change` | `replication_rule.update` | `updateReplicationRule` | M2 |
| `config_change` | `repository.create` | `createRepository` | M1 |
| `config_change` | `repository.delete` | `deleteRepository` | M1 |
| `config_change` | `repository.invalidate_cache` | `invalidateRepositoryCache` | M1 |
| `config_change` | `repository.set_blocked` | `setRepositoryBlocked` | M1 |
| `config_change` | `repository.update` | `patchRepository` | M1 |
| `config_change` | `repository.upstream_check` | `checkProxyUpstream` | M1 |
| `config_change` | `retention_policy.create` | `createRetentionPolicy` | M2 |
| `config_change` | `retention_policy.delete` | `deleteRetentionPolicy` | M2 |
| `config_change` | `retention_policy.dry_run` | `dryRunRetentionPolicy` | M2 |
| `config_change` | `retention_policy.update` | `updateRetentionPolicy` | M2 |
| `config_change` | `routing_rule.create` | `createRoutingRule` | M1 |
| `config_change` | `routing_rule.delete` | `deleteRoutingRule` | M1 |
| `config_change` | `routing_rule.update` | `updateRoutingRule` | M1 |
| `config_change` | `scan.scan_all` | `scanAll` | M2 |
| `config_change` | `scan.stop_all` | `stopAllScans` | M2 |
| `config_change` | `scanner.create` | `createScanner` | M2 |
| `config_change` | `scanner.delete` | `deleteScanner` | M2 |
| `config_change` | `scanner.test` | `testScanner` | M2 |
| `config_change` | `scanner.update` | `updateScanner` | M2 |
| `config_change` | `settings.email_verify` | `verifyEmailSettings` | M2 |
| `config_change` | `settings.update` | `updateSettingsSection` | M1 |
| `config_change` | `task.create` | `createTask` | M2 |
| `config_change` | `task.delete` | `deleteTask` | M2 |
| `config_change` | `task.run` | `runTask` | M2 |
| `config_change` | `task.stop` | `stopTaskRun` | M2 |
| `config_change` | `task.update` | `updateTask` | M2 |
| `config_change` | `trust_policy.create` | `createTrustPolicy` | M2 |
| `config_change` | `trust_policy.delete` | `deleteTrustPolicy` | M2 |
| `config_change` | `trust_policy.update` | `updateTrustPolicy` | M2 |
| `config_change` | `trusted_certificate.create` | `createTrustedCertificate` | M2 |
| `config_change` | `trusted_certificate.delete` | `deleteTrustedCertificate` | M2 |
| `config_change` | `vulnerability.export` | `createVulnerabilityExport` | M2 |
| `config_change` | `webhook.create` | `createWebhook` | M2 |
| `config_change` | `webhook.delete` | `deleteWebhook` | M2 |
| `config_change` | `webhook.redeliver` | `redeliverWebhook` | M2 |
| `config_change` | `webhook.rotate_secret` | `rotateWebhookSecret` | M2 |
| `config_change` | `webhook.test` | `testWebhook` | M2 |
| `config_change` | `webhook.update` | `updateWebhook` | M2 |
| `content_change` | `accessory.delete` | `deleteAccessory` | M2 |
| `content_change` | `asset.delete` | `deleteAsset` | M1 |
| `content_change` | `asset.publish` | `uploadComponent` | M1 |
| `content_change` | `component.delete` | `deleteComponent` | M1 |
| `content_change` | `component.labels_set` | `setComponentLabels` | M2 |
| `content_change` | `oci.tag_delete` | `deleteOciTag` | M2 |
| `content_change` | `oci.tag_put` | `putOciTag` | M2 |
| `content_change` | `promotion.create` | `createPromotion` | M2 |
| `content_change` | `retention_policy.run` | `runRetentionPolicy` | M2 |
| `content_change` | `sbom.generate` | `generateComponentSbom` | M2 |
| `content_change` | `scan.start` | `startScans` | M2 |
| `content_change` | `scan.stop` | `stopScan` | M2 |
| `login_success` | `auth.login` | `login` | M1 |
| `login_success` | `auth.logout` | `logout` | M1 |
| `login_success` | `auth.oidc_login` | `completeOidcLogin` | M1 |
| `rbac_change` | `content_selector.create` | `createContentSelector` | M1 |
| `rbac_change` | `content_selector.delete` | `deleteContentSelector` | M1 |
| `rbac_change` | `content_selector.update` | `updateContentSelector` | M1 |
| `rbac_change` | `idp_group_mapping.create` | `createIdpGroupMapping` | M1 |
| `rbac_change` | `idp_group_mapping.delete` | `deleteIdpGroupMapping` | M1 |
| `rbac_change` | `idp_group_mapping.update` | `updateIdpGroupMapping` | M1 |
| `rbac_change` | `privilege.create` | `createPrivilege` | M1 |
| `rbac_change` | `privilege.delete` | `deletePrivilege` | M1 |
| `rbac_change` | `privilege.update` | `updatePrivilege` | M1 |
| `rbac_change` | `project.member_add` | `addProjectMember` | M2 |
| `rbac_change` | `project.member_remove` | `removeProjectMember` | M2 |
| `rbac_change` | `project.member_update` | `updateProjectMember` | M2 |
| `rbac_change` | `role.create` | `createRole` | M1 |
| `rbac_change` | `role.delete` | `deleteRole` | M1 |
| `rbac_change` | `role.update` | `updateRole` | M1 |
| `rbac_change` | `user.create` | `createUser` | M1 |
| `rbac_change` | `user.delete` | `deleteUser` | M1 |
| `rbac_change` | `user.password_change` | `changeMyPassword` | M1 |
| `rbac_change` | `user.password_reset` | `resetUserPassword` | M1 |
| `rbac_change` | `user.roles_set` | `setUserRoles` | M1 |
| `rbac_change` | `user.update` | `patchUser` | M1 |
| `token_change` | `session.revoke` | `revokeMySession` | M2 |
| `token_change` | `session.revoke_all` | `signOutEverywhere`, `signOutUser` | M1 |
| `token_change` | `token.create` | `createMyToken`, `createRobotToken`, `createProjectRobot` | M1/M2 |
| `token_change` | `token.purge_expired` | `purgeExpiredApiTokens` | M2 |
| `token_change` | `token.revoke` | `revokeMyToken`, `revokeApiToken` | M1 |
| `token_change` | `token.rotate` | `rotateApiToken` | M2 |
| `token_change` | `token.set_enabled` | `setApiTokenEnabled` | M2 |

## 14. Long running operations

Anything that can take longer than a request (garbage collection, scans, replication, lifecycle and retention runs, recounts, exports, SBOM generation, promotion, removing a blob store member, task runs) follows one pattern.

1. `POST` the operation. The answer is **202 Accepted** with the run object in the body (status `queued` or `running`), `Location` pointing at the run (`/api/v1/tasks/{task_id}/runs/{run_id}`, `/api/v1/lifecycle-runs/{run_id}`, `/api/v1/gc/runs/{run_id}`, ...) and `Retry-After` as the polling hint in seconds.
2. `GET` the run until the status is terminal. Statuses: `queued`, `running`, then `succeeded`, `failed`, `cancelled` or `aborted` (the run lost its lock or its node died; `stopped` for scans, GC and replication). The run holds `queued_at`, `started_at`, `finished_at`, a `summary` object, an `error_code` (never raw text) and, where it exists, a log (`GET .../log?from_line=`).
3. Stop with `POST .../actions/stop`: cooperative, takes effect at the next checkpoint, work already done is not undone (GC says so explicitly). A run that already finished answers 409 `conflict`.
4. One active run per key (per task, policy, rule, blob store): a second request is 409 `run_in_progress`. Singleton task kinds take the distributed lock; when another node holds it the run waits `queued`.
5. `dry_run: true` runs only the planning phase and stores a report (summary and a paged candidate list, CSV export). A real run may be bound to a report (`dry_run_id`).
6. Runs and reports are durable (lease queues in PostgreSQL), survive node restarts and are kept for a limited time (task logs 30 days per RET-17, dry run reports by default 30 days, `report_expires_at`); afterwards a report answers 404 and a log says `purged`.
7. Idempotency: these POSTs accept `Idempotency-Key` (section 9).
8. UI behaviour: a toast with a link to the run page; poll every 2 seconds, back off to 15 seconds, stop on a terminal status, pause while the tab is hidden.

Operations that answer 202 (generated):

| Operation | Request | Run resource in the answer | Milestone |
|---|---|---|---|
| `previewCleanupPolicy` | `POST /api/v1/cleanup-policies/{policy_id}/actions/preview` | `LifecycleRun` | M1 |
| `dryRunRetentionPolicy` | `POST /api/v1/retention-policies/{policy_id}/actions/dry-run` | `LifecycleRun` | M2 |
| `runRetentionPolicy` | `POST /api/v1/retention-policies/{policy_id}/actions/run` | `LifecycleRun` | M2 |
| `recountQuota` | `POST /api/v1/quotas/{quota_id}/actions/recount` | `TaskRun` | M2 |
| `startScans` | `POST /api/v1/scans` | `ScanReportList` | M2 |
| `scanAll` | `POST /api/v1/scans/actions/scan-all` | `TaskRun` | M2 |
| `createVulnerabilityExport` | `POST /api/v1/vulnerabilities/exports` | `VulnerabilityExport` | M2 |
| `generateComponentSbom` | `POST /api/v1/components/{component_id}/sbom/actions/generate` | `TaskRun` | M2 |
| `redeliverWebhook` | `POST /api/v1/webhook-deliveries/{delivery_id}/actions/redeliver` | `WebhookDelivery` | M2 |
| `runReplicationRule` | `POST /api/v1/replication-rules/{rule_id}/actions/run` | `ReplicationExecution` | M2 |
| `createPromotion` | `POST /api/v1/promotions` | `Promotion` | M2 |
| `startGcRun` | `POST /api/v1/gc/runs` | `GcRun` | M2 |
| `removeBlobStoreMember` | `DELETE /api/v1/blob-stores/{blob_store_name}/members/{member_name}` | `TaskRun` | M2 |
| `runTask` | `POST /api/v1/tasks/{task_id}/actions/run` | `TaskRun` | M2 |

## 15. Exports: NDJSON and CSV

**Audit export** (`GET /api/v1/audit-events/export`, `application/x-ndjson`):

- UTF-8, one JSON object per line, each line ends with `\n`, no envelope, no array, no trailer. Each line is an `AuditExportRecord`: the audit event plus `cursor`.
- Oldest first. The filters of the list endpoint apply. The range is at most 366 days (400 `range_too_large`, `details.max_days`).
- Streaming with backpressure: the node reads from PostgreSQL as fast as the client consumes and holds no result set in memory. Status 200 and the headers are sent before the first row, so a failure in the middle cannot change the status: the connection is closed without the final newline. A reader treats an unterminated last line or a premature close as a failed export and resumes with `after=<cursor of the last complete line>`.
- Headers: `Content-Type: application/x-ndjson`, `Content-Disposition: attachment; filename=audit-<from>-<to>.ndjson`, `Cache-Control: no-store`, `X-Request-Id`. The export itself is audited (`audit.export`). Very large exports as a background job arrive with UI-M2 (a 202 run with a one time download).

Example (two lines, wrapped here for reading):

```json
{"id":"0192f6c4-5a1e-7000-8000-0000000000a1","at":"2026-10-24T09:15:30Z","kind":"rbac_change","actor_id":"0192f6c4-5a1e-7000-8000-000000000005","actor_name":"alice","client_ip":"10.1.2.3","request_id":"0192f6c4-5a1e-7000-8000-00000000c0de","action":"role.update","target":"maven-publishers","outcome":"success","detail":{"added_privileges":["maven-releases-write"]},"cursor":"eyJhdCI6IjIwMjYtMTAtMjRUMDk6MTU6MzBaIn0"}
{"id":"0192f6c4-5a1e-7000-8000-0000000000a2","at":"2026-10-24T09:16:02Z","kind":"login_failure","actor_id":null,"actor_name":"bob","client_ip":"10.1.2.4","request_id":"0192f6c4-5a1e-7000-8000-00000000c0df","action":"auth.login","target":null,"outcome":"failure","detail":{},"cursor":"eyJhdCI6IjIwMjYtMTAtMjRUMDk6MTY6MDJaIn0"}
```

**CSV exports** (`GET /lifecycle-runs/{run_id}/candidates` with `Accept: text/csv`, vulnerability export download): RFC 4180, UTF-8 without BOM, `\r\n` line ends, a header row with the field names of the JSON representation, `Content-Disposition: attachment`. The whole list is streamed and the paging parameters are ignored. A vulnerability export file is removed after its first download.

## 16. UI page to operation map

Pages and routes come from [ui-page-inventory.md](ui-page-inventory.md). `M1`, `M2`, `M3` are the UI milestones. Operation ids are the `operationId` values of the YAML; the first column groups the pages the way the navigation does. Every operation of the YAML appears at least once.

### 16.1 Sign in, shell and account

| Page | Route | M | Operations | Note |
|---|---|---|---|---|
| Sign in | `/login` | M1 | `getLoginProviders`, `login`, `startOidcLogin`, `completeOidcLogin`, `getSession`, `getMe` | SSO is a browser navigation, not an XHR |
| Shell (every page) | `all` | M1 | `getMe`, `getSession`, `refreshSession`, `getCsrfToken`, `logout`, `getSystemInfo`, `getSystemStatus`, `listRepositories`, `searchComponents`, `listTasks` | header search, command palette, banners (status and jobs only for administrators), session keep-alive |
| Dashboard | `/` | M1 | `getDashboardSummary`, `getMetricsSummary` | one call; the widgets are filtered by privilege server side |
| Profile | `/me` | M1 | `getMe`, `changeMyPassword`, `signOutEverywhere` | password section only for local accounts |
| Profile, sessions | `/me` | M2 | `listMySessions`, `revokeMySession` | needs a session registry |
| Personal tokens | `/me/tokens` | M1 | `listMyTokens`, `createMyToken`, `revokeMyToken`, `getClientSetup` | secret dialog shows the create response once |
| Preferences | `/me/preferences` | M1 | none | stored in the browser, no API |
| About | `/about` | M1 | `getSystemInfo` | anonymous |
| API explorer | `/api-docs` | M3 | `getOpenApiDocument` | UI-M3; the document is served already |

### 16.2 Repositories, browse, search and upload

| Page | Route | M | Operations | Note |
|---|---|---|---|---|
| Repository list | `/repositories` | M1 | `listRepositories`, `getClientSetup`, `patchRepository`, `deleteRepository` | `include=stats` for size and counts |
| Create repository | `/repositories/new` | M1 | `listFormats`, `listBlobStores`, `listCleanupPolicies`, `listRoutingRules`, `listRepositories`, `checkProxyUpstream`, `createRepository` | name check by `q`, proxy probe is a warning only |
| Create repository, registry endpoint | `/repositories/new` | M2 | `listRegistries` | proxy bound to a registry endpoint |
| Repository overview | `/repositories/:name` | M1 | `getRepository`, `listComponents`, `listAuditEvents`, `getClientSetup`, `patchRepository` | online switch is a merge patch |
| Browse | `/repositories/:name/browse` | M1 | `getRepositoryTree`, `getComponent`, `getAsset`, `deleteComponent`, `deleteAsset` | one folder per call, folders first |
| Components | `/repositories/:name/components` | M1 | `listComponents`, `searchComponents` |  |
| Repository settings | `/repositories/:name/settings` | M1 | `getRepository`, `patchRepository`, `listCleanupPolicies`, `listRoutingRules`, `invalidateRepositoryCache`, `setRepositoryBlocked`, `checkProxyUpstream`, `deleteRepository` | If-Match on every patch, 409 lists group members |
| Repository activity | `/repositories/:name/activity` | M1 | `listAuditEvents` | `repository=` filter |
| Repository health | `/repositories/:name/health` | M2 | `getRepositoryHealth`, `listScans`, `searchVulnerabilities`, `startScans` |  |
| Component detail | `/components/:id` | M1 | `getComponent`, `deleteComponent` |  |
| Component detail, full | `/components/:id` | M2 | `listComponentLabels`, `setComponentLabels`, `listComponentVulnerabilities`, `getComponentVulnerabilitySummary`, `getComponentSbom`, `downloadComponentSbom`, `generateComponentSbom`, `listComponentAccessories`, `createPromotion`, `getPromotion` |  |
| Asset detail | `/assets/:id` | M1 | `getAsset`, `listAssets`, `deleteAsset` | `check_blob=true` for the integrity warning |
| OCI image page | `/repositories/:name/images/:image` | M1 | `listComponents`, `getComponent`, `getClientSetup`, `deleteComponent` | basic: tags and digests |
| OCI image page, full | `/repositories/:name/images/:image` | M2 | `listOciTags`, `putOciTag`, `deleteOciTag`, `listComponentAccessories`, `setComponentLabels` |  |
| OCI artifact page | `/repositories/:name/artifacts/:digest` | M2 | `getComponent`, `getOciConfig`, `listOciChildren`, `listOciTags`, `listComponentAccessories`, `deleteAccessory`, `getComponentVulnerabilitySummary`, `listComponentVulnerabilities`, `getComponentSbom`, `generateComponentSbom`, `startScans`, `createPromotion` |  |
| Search | `/search` | M1 | `searchComponents`, `searchAssets`, `listRepositories` | labels filter UI-M2 (`listLabels`) |
| Upload | `/upload` | M1 | `listRepositories`, `listFormats`, `getUploadSchema`, `uploadComponent` | multipart, streamed |
| Client setup panel | `panel` | M1 | `getClientSetup`, `createMyToken` | one panel reused in four places |

### 16.3 Projects and security center

| Page | Route | M | Operations | Note |
|---|---|---|---|---|
| Project list and create | `/projects`, `/projects/new` | M2 | `listProjects`, `createProject`, `listQuotas`, `listRepositories`, `listRegistries` |  |
| Project summary | `/projects/:p` | M2 | `getProject`, `getProjectSummary`, `getProjectQuota` |  |
| Project repositories | `/projects/:p/repositories` | M2 | `listProjectRepositories`, `attachProjectRepository`, `detachProjectRepository`, `createRepository` |  |
| Project members | `/projects/:p/members` | M2 | `listProjectMembers`, `addProjectMember`, `updateProjectMember`, `removeProjectMember`, `listUsers`, `listIdpGroupMappings` |  |
| Project labels | `/projects/:p/labels` | M2 | `listLabels`, `getLabel`, `createLabel`, `updateLabel`, `deleteLabel` |  |
| Project policies | `/projects/:p/policies` | M2 | `listRetentionPolicies`, `getRetentionPolicy`, `createRetentionPolicy`, `updateRetentionPolicy`, `deleteRetentionPolicy`, `dryRunRetentionPolicy`, `runRetentionPolicy`, `listLifecycleRuns`, `getLifecycleRun`, `listLifecycleCandidates`, `listImmutabilityRules`, `getImmutabilityRule`, `createImmutabilityRule`, `updateImmutabilityRule`, `deleteImmutabilityRule`, `testImmutabilityRules` | dry run report viewer is shared |
| Project security | `/projects/:p/security` | M2 | `listScanners`, `listCveAllowlist`, `addCveAllowlistEntries`, `deleteCveAllowlistEntry`, `copySystemCveAllowlist`, `listTrustPolicies`, `createTrustPolicy`, `updateTrustPolicy`, `deleteTrustPolicy`, `patchProject` |  |
| Project robot tokens | `/projects/:p/robots` | M2 | `listProjectRobots`, `createProjectRobot`, `rotateApiToken`, `setApiTokenEnabled`, `revokeApiToken` |  |
| Project webhooks | `/projects/:p/webhooks` | M2 | `listWebhooks`, `createWebhook`, `updateWebhook`, `deleteWebhook`, `testWebhook`, `rotateWebhookSecret`, `listWebhookDeliveries`, `redeliverWebhook` |  |
| Project logs | `/projects/:p/logs` | M2 | `listAuditEvents` | `project=` filter |
| Project settings | `/projects/:p/settings` | M2 | `getProject`, `patchProject`, `getProjectQuota`, `updateQuota`, `deleteProject` |  |
| Security overview | `/security` | M2 | `getSecurityOverview`, `listScans` |  |
| Vulnerability search | `/security/vulnerabilities` | M2 | `searchVulnerabilities`, `createVulnerabilityExport`, `getVulnerabilityExport`, `downloadVulnerabilityExport` | CVE id is required |
| Scan activity | `/security/scans` | M2 | `listScans`, `getScan`, `scanAll`, `stopAllScans`, `stopScan`, `getScanLog`, `listTasks` |  |

### 16.4 Administration

| Page | Route | M | Operations | Note |
|---|---|---|---|---|
| Cleanup policies | `/admin/cleanup-policies` | M1 | `listCleanupPolicies`, `createCleanupPolicy`, `getCleanupPolicy`, `updateCleanupPolicy`, `deleteCleanupPolicy`, `previewCleanupPolicy`, `listLifecycleRuns`, `getLifecycleRun`, `listLifecycleCandidates`, `listFormats` | preview is a stored dry run, CSV export of the plan |
| Routing rules | `/admin/routing-rules` | M1 | `listRoutingRules`, `createRoutingRule`, `getRoutingRule`, `updateRoutingRule`, `deleteRoutingRule`, `validateRoutingPatterns`, `testDraftRoutingRule`, `testRoutingRule` |  |
| Content selectors | `/admin/content-selectors` | M1 | `listContentSelectors`, `createContentSelector`, `getContentSelector`, `updateContentSelector`, `deleteContentSelector`, `validateContentSelector`, `testContentSelector`, `previewContentSelector`, `getSelectorLanguage` |  |
| Blob stores | `/admin/blob-stores`, `/admin/blob-stores/:name` | M1 | `listBlobStores`, `createBlobStore`, `getBlobStore`, `patchBlobStore`, `deleteBlobStore`, `getBlobStoreUsage`, `testBlobStoreConnection` |  |
| Blob store groups | `/admin/blob-stores/:name` | M2 | `listBlobStoreMembers`, `setBlobStoreMembers`, `removeBlobStoreMember`, `convertBlobStoreToGroup` |  |
| Garbage collection | `/admin/gc` | M2 | `listGcRuns`, `getGcRun`, `startGcRun`, `stopGcRun`, `listGcCandidates`, `listTasks`, `updateTask` | schedule is a system task |
| Users | `/admin/users`, `/admin/users/:id` | M1 | `listUsers`, `getUser`, `createUser`, `patchUser`, `deleteUser`, `setUserRoles`, `resetUserPassword`, `signOutUser`, `listApiTokens`, `listRoles` |  |
| Roles | `/admin/roles` | M1 | `listRoles`, `createRole`, `getRole`, `updateRole`, `deleteRole`, `getRoleEffectivePrivileges`, `listPrivileges` |  |
| Privileges | `/admin/privileges` | M1 | `listPrivileges`, `createPrivilege`, `getPrivilege`, `updatePrivilege`, `deletePrivilege`, `listContentSelectors`, `listRepositories` |  |
| Group mappings | `/admin/group-mappings` | M1 | `listIdpGroupMappings`, `createIdpGroupMapping`, `updateIdpGroupMapping`, `deleteIdpGroupMapping`, `testIdpGroupMappings`, `listRoles` | tester resolves a group list |
| Authentication providers | `/admin/auth/providers` | M1 | `getAuthProvidersDetails` | read only view of the file configuration |
| Authentication providers, editing | `/admin/auth/providers` | M2 | `updateOidcProvider`, `updateLdapProvider`, `verifyLdapProvider` |  |
| Anonymous access | `/admin/anonymous` | M1 | `getAnonymousAccess`, `updateAnonymousAccess`, `setAnonymousRepositories`, `listRepositories` |  |
| API tokens (all) | `/admin/tokens` | M1 | `listApiTokens`, `getApiToken`, `revokeApiToken` |  |
| API tokens, policy and purge | `/admin/tokens` | M2 | `purgeExpiredApiTokens`, `getSettingsSection`, `updateSettingsSection`, `createRobotToken` |  |
| Tasks | `/admin/tasks` | M1 | `listTasks`, `getTask`, `listTaskRuns` | read only |
| Task run | `/admin/tasks/:id/runs/:run` | M1 | `getTaskRun`, `getTaskRunLog` | log follow by polling |
| Tasks, editing | `/admin/tasks` | M2 | `createTask`, `updateTask`, `deleteTask`, `runTask`, `stopTaskRun` |  |
| Webhooks (system) | `/admin/webhooks` | M2 | `listWebhooks`, `createWebhook`, `getWebhook`, `updateWebhook`, `deleteWebhook`, `testWebhook`, `rotateWebhookSecret`, `listWebhookDeliveries`, `redeliverWebhook` |  |
| Registries | `/admin/registries` | M2 | `listRegistries`, `createRegistry`, `getRegistry`, `updateRegistry`, `deleteRegistry`, `testRegistry` |  |
| Replication | `/admin/replication` | M2 | `listReplicationRules`, `createReplicationRule`, `getReplicationRule`, `updateReplicationRule`, `deleteReplicationRule`, `runReplicationRule`, `listReplicationExecutions`, `getReplicationExecution`, `listReplicationTasks`, `stopReplicationExecution` |  |
| Scanners | `/admin/scanners` | M2 | `listScanners`, `createScanner`, `getScanner`, `updateScanner`, `deleteScanner`, `testScanner` |  |
| CVE allowlist (system) | `/admin/cve-allowlist` | M2 | `listCveAllowlist`, `addCveAllowlistEntries`, `deleteCveAllowlistEntry` |  |
| Trust policies | `/admin/trust-policies` | M2 | `listTrustPolicies`, `createTrustPolicy`, `getTrustPolicy`, `updateTrustPolicy`, `deleteTrustPolicy` |  |
| Quotas | `/admin/quotas` | M2 | `listQuotas`, `getQuota`, `updateQuota`, `recountQuota`, `listProjects` |  |
| System settings | `/admin/settings` | M1 | `getSettingsSchema`, `getSettingsSection`, `updateSettingsSection` | mostly read only in M1 |
| Email | `/admin/settings/email` | M2 | `getSettingsSection`, `updateSettingsSection`, `verifyEmailSettings` |  |
| HTTP proxy | `/admin/settings/http-proxy` | M2 | `getSettingsSection`, `updateSettingsSection`, `listTrustedCertificates` |  |
| Certificates | `/admin/settings/certificates` | M2 | `listTrustedCertificates`, `createTrustedCertificate`, `deleteTrustedCertificate` |  |
| Audit log | `/admin/audit` | M1 | `listAuditEvents`, `getAuditEvent`, `exportAuditEvents` | NDJSON export is a download |
| Status | `/admin/system/status` | M1 | `getSystemStatus` |  |
| Nodes | `/admin/system/nodes` | M2 | `listNodes` |  |
| Info | `/admin/system/info` | M2 | `getSystemInfoDetails` |  |


## 17. Operation inventory

The YAML holds 251 operations (115 for UI-M1, 136 for UI-M2), 285 component schemas and 13 shared error responses. Counts per tag (the first tag of each operation):

| Tag | UI-M1 | UI-M2 | Total |
|---|---|---|---|
| API tokens | 3 | 0 | 3 |
| Anonymous access | 3 | 0 | 3 |
| Assets | 3 | 0 | 3 |
| Audit | 3 | 0 | 3 |
| Auth | 9 | 3 | 12 |
| Blob stores | 7 | 4 | 11 |
| Browse | 1 | 0 | 1 |
| CVE allowlists | 0 | 4 | 4 |
| Certificates | 0 | 3 | 3 |
| Cleanup policies | 6 | 0 | 6 |
| Client setup | 1 | 0 | 1 |
| Components | 3 | 0 | 3 |
| Content selectors | 9 | 0 | 9 |
| Dashboard | 1 | 0 | 1 |
| Formats | 2 | 0 | 2 |
| Garbage collection | 0 | 5 | 5 |
| IdP group mappings | 5 | 0 | 5 |
| Immutability | 0 | 6 | 6 |
| Labels | 0 | 7 | 7 |
| Lifecycle runs | 3 | 0 | 3 |
| Me | 6 | 2 | 8 |
| Nodes | 0 | 1 | 1 |
| OCI artifacts | 0 | 5 | 5 |
| Privileges | 5 | 0 | 5 |
| Project members | 0 | 4 | 4 |
| Projects | 0 | 9 | 9 |
| Promotion | 0 | 2 | 2 |
| Quotas | 0 | 5 | 5 |
| Registries | 0 | 6 | 6 |
| Replication | 0 | 10 | 10 |
| Repositories | 8 | 1 | 9 |
| Retention | 0 | 7 | 7 |
| Robot tokens | 0 | 6 | 6 |
| Roles | 6 | 0 | 6 |
| Routing rules | 8 | 0 | 8 |
| SBOM | 0 | 5 | 5 |
| Scanners | 0 | 6 | 6 |
| Scans | 0 | 7 | 7 |
| Search | 2 | 0 | 2 |
| Settings | 3 | 1 | 4 |
| System | 4 | 1 | 5 |
| Tasks | 5 | 5 | 10 |
| Trust policies | 0 | 5 | 5 |
| Upload | 1 | 0 | 1 |
| Users | 8 | 0 | 8 |
| Vulnerabilities | 0 | 7 | 7 |
| Webhooks | 0 | 9 | 9 |
| **All** | **115** | **136** | **251** |

## 18. Decisions where the documents were silent

These were decided while writing the contract. Each can be reversed in the YAML and here without touching the others.

| # | Decision | Reason |
|---|---|---|
| 1 | Paths in the YAML carry the full `/api/v1` prefix and the server entry is `/`. | One copy of the prefix in the generated client, and a grep for a route finds the YAML line. |
| 2 | Validation failures are 400 `invalid_input` with `details.fields`, not 422. | `DomainError::Invalid` maps to 400 in `rustryx-api`; the contract follows the implementation. |
| 3 | The envelope gets an optional `details` object, and 28 codes are added (section 5.2, origin `contract`). | The UI needs dependents on 409, field errors, selector compile errors and a stable code per refusal; `DomainError::Conflict` alone always renders `conflict`. |
| 4 | `PUT` and `PATCH` of objects require `If-Match` (428 without it); the ETag covers writable state only. | UI principle 8; core entities have no version column. |
| 5 | PATCH (merge patch) for repositories, blob stores, users and projects; PUT (full replace) for the rest. | The first group holds write only secrets that are never echoed, so a full replace would force clients to resend them. |
| 6 | Repositories and blob stores are addressed by name, everything else by UUID. | Names are the identity in UI routes and they never change. |
| 7 | In phase 1 `x-required-privilege` is `nx-admin` for every administration operation, `x-future-privilege` names the AUTH-46 area privilege. | Handlers switch without a contract change. |
| 8 | Token creation, password change, session refresh and session list are `interactive`: `ryx_*` tokens are refused. | A leaked token must not make itself permanent. |
| 9 | Cookie names `__Host-rustryx_session` and `__Host-rustryx_csrf`; signed double submit plus `Origin` check; `GET /auth/csrf` to recover; transparent reissue plus `POST /auth/session/refresh`. | SEC-36, README 8.3 and auth-core ADR-2 fix the rules, not the shapes. |
| 10 | The OIDC callback answers a browser with a 302 to `/login?error=<code>` instead of JSON. | A top level navigation cannot show an envelope. |
| 11 | `return_to` of the OIDC login is sealed in the state cookie and must be a plain absolute path. | Open redirect protection. |
| 12 | The global anonymous switch is read only in UI-M1 (`global_editable: false`, 409 `setting_managed_by_config`). | It is `auth.anonymous_enabled` in the configuration; the endpoint is ready for a runtime setting. |
| 13 | S3 blob store credentials are write only inline values (sealed) or `env:` and `file:` references, never returned; the view shows `configured` and `source`. | Parity doc asks for secret references; inline values are the easy path for the UI. |
| 14 | Deleting a repository is blocked while it is a group member (409 with the groups) and while it holds components unless `include_content=true`. | A destructive call must name its consequence. |
| 15 | Deleting a role, privilege, cleanup policy or label that is in use is 409 `resource_in_use` with dependents; `force=true` detaches. A routing rule or a content selector in use has no force. | UI inventory ("role in use on delete", "delete blocked while assigned"). |
| 16 | An upload refused by the write policy is 409 `write_policy_violation` for both `allow_once` and `deny` (`details.policy`). | One status the form can explain; format-core uses 403 for `deny` on native protocols. |
| 17 | Search reaches the first 1000 hits and needs 3 characters before a trailing `*`; no total counts anywhere. | SEC-33 (heavy queries), keyset pagination. |
| 18 | The audit export is a synchronous NDJSON stream with a cursor per line and a 366 day range; the background job variant is UI-M2. | SIEM friendly and no run object to poll for the common case. |
| 19 | `idempotency` keys last 24 hours, are scoped to principal and route, and are not offered for operations that return a secret. | Replay would mean storing the secret. |
| 20 | The list of active sessions and per session revoke are UI-M2. | Sessions are stateless JWTs; a registry table does not exist in `rustryx-db`. Sign out everywhere (`tokens_valid_after`) is UI-M1. |
| 21 | Settings pages of UI-M1 are mostly read only (values from configuration, `sources` per field); `PUT` is real for runtime settings. | Config precedence is environment, file, default; nothing may silently override it. |
| 22 | `GET /system/metrics/summary` is per node (counters since process start). | Fleet wide numbers belong to Prometheus. |
| 23 | Client setup is per repository (`GET /client-setup?repository=`), with the hosted publish target named for groups. | UI inventory section 9. |
| 24 | `TreeNode.path` has a leading `/` (the form content selectors see) while `Asset.path` has none (core). | Selectors and the tree share one path form. |
| 25 | The cleanup policy carries one `format` and a criteria object (all set criteria must match, at least one required); formats declare the supported criteria in `GET /formats`. | Parity 12.2. |
| 26 | The upstream probe for the create wizard is `POST /upstream-checks` (always 200, outcome in the body). | A failed probe is a warning, saving stays possible (UI inventory). |
| 27 | A refused or failed security relevant audit write fails the operation closed (503); content audit never undoes an upload. | SEC-39 to SEC-41 and format-core. |

## 19. Conflicts with rustryx-core and rustryx-api

Nothing in core or `rustryx-api` was changed. These are the places where the contract needs a change there (the core gaps in [core-gaps.md](core-gaps.md) are referenced by number).

| # | Where | What conflicts | Needed |
|---|---|---|---|
| 1 | core `model.rs`, `identity.rs`, `audit.rs` (`time` crate) | `time` is built with `serde` only, so `OffsetDateTime` serializes as the crate's tuple form, not RFC 3339. | Serialize through DTOs or add `#[serde(with = "time::serde::rfc3339")]` (feature `serde-well-known`) before any core entity is returned directly. |
| 2 | core `Secret`, `ApiToken.token_hash`, `ProxyConfig.auth.password` | These serialize real values (core-gaps 34). The contract returns redacted views (`UpstreamAuth.password_set`, no `token_hash`). | A test in `rustryx-api` that no response DTO embeds `Secret` or `token_hash`. |
| 3 | `rustryx-api` `ApiError` and `DomainError` | No `details`; `Conflict` and `AlreadyExists` always render `conflict` and `already_exists`; the codes of section 5.2 with origin `contract` do not exist. `Invalid` is 400 (the UI inventory expects 422). | Add `details` to the envelope and typed domain errors (in use, builtin, cycle, policy, ...) that carry a code and details. |
| 4 | core `StoreError` | No invalid-input variant: a malformed page token is `Conflict` (409) (core-gaps 1 and 18). The contract says 400 `invalid_page_token`. | Add `StoreError::Invalid`. |
| 5 | `rustryx-api` `auth.rs` | Credentials are read from the `Authorization` header only, by design. There is no cookie extractor, no CSRF token or `Origin` check, no session reissue and no `interactive` gate. | A cookie credential path, the CSRF layer of section 11.3 and an `AuthMethod` check for `interactive` operations. |
| 6 | `rustryx-api` `layers.rs` CORS | Allows no credentials and only `Authorization`, `Content-Type`, `X-Request-Id`; exposes only `X-Request-Id` and `Retry-After`. | Nothing while the SPA is same origin; for a cross origin dev server add `If-Match`, `Idempotency-Key`, `X-CSRF-Token`, `ETag`, `Location`. |
| 7 | `rustryx-api` `layers.rs` CSP | The default `Content-Security-Policy: default-src 'none'; sandbox` is set on every response unless the handler sets its own. | The embedded SPA must send its own CSP (strict, no inline script). |
| 8 | core `Principal`, `User` | No email, display name, external id on `Principal` (core-gaps 16, 25); `User` is not serializable and has no `last_login_at`. `/me`, `UserView` and JIT need them. | Extend `Principal`; add a login timestamp. |
| 9 | core `RbacChecker`, `Principal::is_admin` | No `is_admin` on the port (core-gaps 14), and the name based `Principal::is_admin` is untrusted (15). The `nx-admin` gate must match the role id. | Use `Rbac::is_admin`; add it to the port. |
| 10 | core `ApiToken`, `rustryx-auth` `TokenSpec` | No `kind`, project binding or `disabled`; lifetime is mandatory (at most 365 days), so the never-expire robot option has no home. | UI-M2 columns and the setting that allows never-expire for `nx-admin`. |
| 11 | core `AuditEvent`, `AuditSink` | The event has no `id` (the table has) and the sink is write only: no filters, keyset or export (core-gaps 19). No kinds for logout and access denied. | `AuditReadRepo`, an `id`, new kinds. |
| 12 | core ports | No entities or ports for cleanup policies, routing rules (core-gaps 19, 27), tasks and runs (only `PgJobLeases`), lifecycle runs, settings (`PgSystemConfig` is inherent), session registry, upstream status. | New entities and ports in wave 2. |
| 13 | core `BlobStoreRepo`, `BlobRepo`, `BlobStoreKind` | No update (core-gaps 10), no blob counts or free space, kinds are `local` and `s3` only. | `update`, usage queries; `azure` and `group` with UI-M2. |
| 14 | `rustryx-auth` OIDC flow | The sealed flow data holds `state`, `nonce` and `verifier` only (no `return_to`); a session reissue needs the Keycloak refresh token handled somewhere (auth-core ADR-2); "no mapped role" is reported as `GroupLookupFailed` (core-gaps 20). | `return_to` in the flow data, a decision where the refresh token lives, `AuthError::AccessDenied`. |
| 15 | core `Coordinate` | No `extension` or `classifier` (core-gaps 12), so selector samples and `coordinate.extension` are limited to group, name, version. | Add the fields. |
| 16 | core `PageRequest` | No sort and no filters; the database orders users and components by id and assets by path, and its page token is not bound to filters. The contract's `sort` lists and filters need ports and indexes. | Extend the list ports per resource. |
| 17 | `rustryx-api` | `GET /api/v1/openapi.json` is not served; no `Idempotency-Key` store; `X-Request-Id` and the rest are fine. | Serve the embedded YAML as JSON; an idempotency table with a 24 hour TTL. |

## 20. Maintaining the contract

- Run `docs/openapi/check.sh` before every change to the YAML. It lints with Redocly (pinned version, `npx`), then checks the house rules: every operation has an `operationId`, tags, a summary, `x-milestone`, an `x-required-privilege` in the grammar above, an explicit `security` list, 401, 403, 429 and 503 responses and an error response that resolves to the shared `Error` envelope; PUT and PATCH have `If-Match` (except the listed ones); no secret appears in a response schema outside `ApiTokenCreated`, `WebhookCreated`, the CSRF token and the `token` summary of `/me`; no em dash or en dash in the YAML or this document; and, when `jsonschema` is installed, every example validates against its schema. Two warnings are expected and allowed: the OIDC login and callback answer 302 by design.
- Adding an operation: pick the tag and the `operationId` (`verbNoun`), set `x-milestone`, `x-required-privilege` (and `x-future-privilege` for administration), `x-audit` for state changes, reuse `$ref` parameters and responses, add request and response schemas under `components` with an `example`, and add the page to the map in section 16.
- Add a schema field only as optional or nullable; never reuse a field name for another meaning.
- The UI client is generated from the YAML (for example with `openapi-typescript`); UI work against a mock (`prism mock`) needs nothing else. Backend handlers are tested against the document: a contract test calls each implemented operation on a real node and validates the response with the schema (`schemathesis` or equivalent), and CI fails when a handler answers with an undocumented status or code.
- The YAML is plain hand-maintained YAML; no generator is part of the repository.
