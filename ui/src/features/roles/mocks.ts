import { http, HttpResponse, type HttpHandler } from 'msw'
import {
  checkOptionalIfMatch,
  handle,
  jsonWithEtag,
  MockProblem,
  noContent,
  pageResponse,
  readJson,
  requireIfMatch,
  slice,
  startsWithText,
} from './mock-http'
import {
  audit,
  commit,
  effectivePrivileges,
  etagOfRole,
  findRole,
  freshId,
  identity,
  roleView,
  validateRoleInput,
  type RoleInput,
} from './mock-store'

const diff = (before: readonly string[], after: readonly string[]) => ({
  added: after.filter((x) => !before.includes(x)),
  removed: before.filter((x) => !after.includes(x)),
})

/** Stateful mock of the roles API: validation, ETags, cycles, in-use deletes, audit records. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/roles', ({ request }) =>
    handle(request, { feature: 'roles' }, () => {
      const url = new URL(request.url)
      const q = url.searchParams.get('q')
      const builtin = url.searchParams.get('builtin')
      const roles = identity()
        .roles.filter((r) => (q === null ? true : startsWithText(r.name, q)))
        .filter((r) => (builtin === null ? true : r.builtin === (builtin === 'true')))
        .sort((a, b) => a.name.localeCompare(b.name))
        .map(roleView)
      return pageResponse(slice(roles, url))
    }),
  ),

  http.post('/api/v1/roles', ({ request }) =>
    handle(request, { feature: 'roles' }, async (caller) => {
      const input = validateRoleInput(await readJson<RoleInput>(request), null)
      const role = { id: freshId(), builtin: false, ...input }
      commit({ roles: [...identity().roles, role] })
      audit(caller, { kind: 'rbac_change', action: 'role.create', target: role.name })
      return jsonWithEtag(roleView(role), etagOfRole(role), 201)
    }),
  ),

  http.get('/api/v1/roles/:roleId', ({ request, params }) =>
    handle(request, { feature: 'roles' }, () => {
      const role = findRole(String(params.roleId))
      return jsonWithEtag(roleView(role), etagOfRole(role))
    }),
  ),

  http.put('/api/v1/roles/:roleId', ({ request, params }) =>
    handle(request, { feature: 'roles' }, async (caller) => {
      const role = findRole(String(params.roleId))
      requireIfMatch(request, etagOfRole(role))
      if (role.builtin) {
        throw new MockProblem(409, 'builtin_read_only', 'Built-in roles cannot be changed')
      }
      const input = validateRoleInput(await readJson<RoleInput>(request), role.id)
      const next = { ...role, ...input }
      commit({ roles: identity().roles.map((r) => (r.id === role.id ? next : r)) })
      audit(caller, {
        kind: 'rbac_change',
        action: 'role.update',
        target: next.name,
        detail: {
          added_privileges: diff(role.privilege_ids, next.privilege_ids).added,
          removed_privileges: diff(role.privilege_ids, next.privilege_ids).removed,
        },
      })
      return jsonWithEtag(roleView(next), etagOfRole(next))
    }),
  ),

  http.delete('/api/v1/roles/:roleId', ({ request, params }) =>
    handle(request, { feature: 'roles' }, (caller) => {
      const role = findRole(String(params.roleId))
      checkOptionalIfMatch(request, etagOfRole(role))
      if (role.builtin) {
        throw new MockProblem(409, 'builtin_read_only', 'Built-in roles cannot be deleted')
      }
      const force = new URL(request.url).searchParams.get('force') === 'true'
      const state = identity()
      const users = state.users.filter((u) => u.manualRoleIds.includes(role.id)).length
      const mappings = state.mappings.filter((m) => m.role_id === role.id).length
      const parents = state.roles.filter((r) => r.nested_role_ids.includes(role.id))
      if (!force && users + mappings + parents.length > 0) {
        throw new MockProblem(409, 'resource_in_use', 'The role is still in use', {
          details: {
            dependents: [
              ...(users > 0 ? [{ kind: 'user', id: null, name: `${users} users` }] : []),
              ...(mappings > 0
                ? [{ kind: 'idp_group_mapping', id: null, name: `${mappings} group mappings` }]
                : []),
              ...parents.map((r) => ({ kind: 'role', id: r.id, name: r.name })),
            ],
          },
        })
      }
      commit({
        roles: state.roles
          .filter((r) => r.id !== role.id)
          .map((r) => ({
            ...r,
            nested_role_ids: r.nested_role_ids.filter((n) => n !== role.id),
          })),
        users: state.users.map((u) => ({
          ...u,
          manualRoleIds: u.manualRoleIds.filter((r) => r !== role.id),
          external: u.external.filter((e) => e.roleId !== role.id),
        })),
        mappings: state.mappings.filter((m) => m.role_id !== role.id),
      })
      audit(caller, {
        kind: 'rbac_change',
        action: 'role.delete',
        target: role.name,
        detail: { forced: force },
      })
      return noContent()
    }),
  ),

  http.get('/api/v1/roles/:roleId/effective-privileges', ({ request, params }) =>
    handle(request, { feature: 'roles' }, () =>
      HttpResponse.json({ items: effectivePrivileges(findRole(String(params.roleId))) }),
    ),
  ),
]
