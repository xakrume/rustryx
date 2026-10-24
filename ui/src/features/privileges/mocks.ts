import { http, type HttpHandler } from 'msw'
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
} from '@/features/roles/mock-http'
import {
  audit,
  commit,
  etagOfPrivilege,
  findPrivilege,
  freshId,
  identity,
  privilegeView,
  validatePrivilegeInput,
} from '@/features/roles/mock-store'
import { repositories } from '@/mocks/store'
import type { components } from '@/api/schema'

const repositoryNames = (): string[] => repositories.map((r) => r.name)

/** Stateful mock of the privileges API: validation, ETags, built-in protection, force delete. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/privileges', ({ request }) =>
    handle(request, { feature: 'privileges' }, () => {
      const url = new URL(request.url)
      const q = url.searchParams.get('q')
      const kind = url.searchParams.get('kind')
      const repository = url.searchParams.get('repository')
      const builtin = url.searchParams.get('builtin')
      const items = identity()
        .privileges.filter((p) => (q === null ? true : startsWithText(p.name, q)))
        .filter((p) => (kind === null ? true : p.grant.type === kind))
        .filter((p) =>
          repository === null
            ? true
            : p.grant.type !== 'admin' &&
              (repository === '*'
                ? p.grant.repository === '*'
                : p.grant.repository.toLowerCase().includes(repository.toLowerCase())),
        )
        .filter((p) => (builtin === null ? true : p.builtin === (builtin === 'true')))
        .sort((a, b) => a.name.localeCompare(b.name))
        .map(privilegeView)
      return pageResponse(slice(items, url))
    }),
  ),

  http.post('/api/v1/privileges', ({ request }) =>
    handle(request, { feature: 'privileges' }, async (caller) => {
      const input = validatePrivilegeInput(
        await readJson<Partial<components['schemas']['PrivilegeInput']>>(request),
        null,
        repositoryNames(),
      )
      const privilege = { id: freshId(), builtin: false, ...input }
      commit({ privileges: [...identity().privileges, privilege] })
      audit(caller, { kind: 'rbac_change', action: 'privilege.create', target: privilege.name })
      return jsonWithEtag(privilegeView(privilege), etagOfPrivilege(privilege), 201)
    }),
  ),

  http.get('/api/v1/privileges/:privilegeId', ({ request, params }) =>
    handle(request, { feature: 'privileges' }, () => {
      const privilege = findPrivilege(String(params.privilegeId))
      return jsonWithEtag(privilegeView(privilege), etagOfPrivilege(privilege))
    }),
  ),

  http.put('/api/v1/privileges/:privilegeId', ({ request, params }) =>
    handle(request, { feature: 'privileges' }, async (caller) => {
      const privilege = findPrivilege(String(params.privilegeId))
      requireIfMatch(request, etagOfPrivilege(privilege))
      if (privilege.builtin) {
        throw new MockProblem(409, 'builtin_read_only', 'Built-in privileges cannot be changed')
      }
      const input = validatePrivilegeInput(
        await readJson<Partial<components['schemas']['PrivilegeInput']>>(request),
        privilege.id,
        repositoryNames(),
      )
      const next = { ...privilege, ...input }
      commit({ privileges: identity().privileges.map((p) => (p.id === privilege.id ? next : p)) })
      audit(caller, { kind: 'rbac_change', action: 'privilege.update', target: next.name })
      return jsonWithEtag(privilegeView(next), etagOfPrivilege(next))
    }),
  ),

  http.delete('/api/v1/privileges/:privilegeId', ({ request, params }) =>
    handle(request, { feature: 'privileges' }, (caller) => {
      const privilege = findPrivilege(String(params.privilegeId))
      checkOptionalIfMatch(request, etagOfPrivilege(privilege))
      if (privilege.builtin) {
        throw new MockProblem(409, 'builtin_read_only', 'Built-in privileges cannot be deleted')
      }
      const force = new URL(request.url).searchParams.get('force') === 'true'
      const state = identity()
      const roles = state.roles.filter((r) => r.privilege_ids.includes(privilege.id))
      if (!force && roles.length > 0) {
        throw new MockProblem(409, 'resource_in_use', 'The privilege is included in roles', {
          details: {
            dependents: roles.map((r) => ({ kind: 'role', id: r.id, name: r.name })),
          },
        })
      }
      commit({
        privileges: state.privileges.filter((p) => p.id !== privilege.id),
        roles: state.roles.map((r) => ({
          ...r,
          privilege_ids: r.privilege_ids.filter((p) => p !== privilege.id),
        })),
      })
      audit(caller, {
        kind: 'rbac_change',
        action: 'privilege.delete',
        target: privilege.name,
        detail: { forced: force },
      })
      return noContent()
    }),
  ),
]
