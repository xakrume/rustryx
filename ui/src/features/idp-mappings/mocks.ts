import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import {
  checkOptionalIfMatch,
  handle,
  invalidInput,
  jsonWithEtag,
  MockProblem,
  noContent,
  pageResponse,
  readJson,
  requireIfMatch,
  slice,
  startsWithText,
} from '@/features/roles/mock-http'
import { ROLE_ID, type MappingRecord } from '@/features/roles/mock-seed'
import {
  audit,
  commit,
  etagOfMapping,
  findRole,
  freshId,
  identity,
} from '@/features/roles/mock-store'

type S = components['schemas']

/** The admin groups of the mock configuration, as `/auth/providers/details` reports them. */
const ADMIN_GROUPS: Record<'oidc' | 'ldap', string[]> = { oidc: ['/rustryx/admins'], ldap: [] }

const view = (m: MappingRecord): S['IdpGroupMappingView'] => ({
  ...m,
  role: { id: m.role_id, name: findRole(m.role_id).name },
})

function findMapping(id: string): MappingRecord {
  const mapping = identity().mappings.find((m) => m.id === id)
  if (!mapping) {
    throw new MockProblem(404, 'idp_group_mapping_not_found', 'The group mapping does not exist')
  }
  return mapping
}

function checkRole(roleId: unknown): string {
  const role =
    typeof roleId === 'string' ? identity().roles.find((r) => r.id === roleId) : undefined
  if (!role || role.id === ROLE_ID.admin || role.id === ROLE_ID.anonymous) {
    throw invalidInput([
      {
        field: '/role_id',
        code: 'not_mappable',
        message:
          'nx-admin and nx-anonymous cannot be mapped. Administrators come from the admin groups.',
      },
    ])
  }
  return role.id
}

/** Stateful mock of the group mapping API, including the `GET` by id the contract should offer. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/idp-group-mappings', ({ request }) =>
    handle(request, { feature: 'mappings' }, () => {
      const url = new URL(request.url)
      const provider = url.searchParams.get('provider')
      const roleId = url.searchParams.get('role_id')
      const q = url.searchParams.get('q')
      const items = identity()
        .mappings.filter((m) => provider === null || m.provider === provider)
        .filter((m) => roleId === null || m.role_id === roleId)
        .filter((m) => q === null || startsWithText(m.external_group, q))
        .sort(
          (a, b) =>
            a.provider.localeCompare(b.provider) ||
            a.priority - b.priority ||
            a.external_group.localeCompare(b.external_group),
        )
        .map(view)
      return pageResponse(slice(items, url))
    }),
  ),

  http.post('/api/v1/idp-group-mappings/test', ({ request }) =>
    handle(request, { feature: 'mappings' }, async () => {
      const body = await readJson<{ provider?: 'oidc' | 'ldap'; groups?: string[] }>(request)
      const groups = body.groups ?? []
      if (!body.provider || groups.length < 1 || groups.length > 256) {
        throw invalidInput([
          { field: '/groups', code: 'invalid', message: 'Send 1 to 256 groups.' },
        ])
      }
      const state = identity()
      const admin = findRole(ROLE_ID.admin)
      const roles: S['MappingTestResult']['roles'] = []
      const matched = new Set<string>()
      for (const group of groups) {
        // Exact comparison on purpose: no trim, no case folding.
        if (ADMIN_GROUPS[body.provider].includes(group)) {
          roles.push({
            role: { id: admin.id, name: admin.name },
            via: 'admin_group',
            group,
            mapping_id: null,
          })
          matched.add(group)
        }
      }
      for (const mapping of state.mappings
        .filter((m) => m.provider === body.provider && groups.includes(m.external_group))
        .sort((a, b) => a.priority - b.priority)) {
        roles.push({
          role: { id: mapping.role_id, name: findRole(mapping.role_id).name },
          via: 'mapping',
          group: mapping.external_group,
          mapping_id: mapping.id,
        })
        matched.add(mapping.external_group)
      }
      return HttpResponse.json({
        roles,
        unmatched_groups: groups.filter((g) => !matched.has(g)),
        denied: roles.length === 0,
      })
    }),
  ),

  http.post('/api/v1/idp-group-mappings', ({ request }) =>
    handle(request, { feature: 'mappings' }, async (caller) => {
      const body = await readJson<{
        provider?: 'oidc' | 'ldap'
        external_group?: unknown
        role_id?: unknown
        priority?: unknown
      }>(request)
      const group = typeof body.external_group === 'string' ? body.external_group : ''
      if (
        group === '' ||
        group.length > 512 ||
        (body.provider !== 'oidc' && body.provider !== 'ldap')
      ) {
        throw invalidInput([
          {
            field: '/external_group',
            code: 'invalid',
            message: 'Enter the group, up to 512 characters.',
          },
        ])
      }
      const roleId = checkRole(body.role_id)
      if (
        identity().mappings.some((m) => m.provider === body.provider && m.external_group === group)
      ) {
        throw new MockProblem(
          409,
          'already_exists',
          'This group is already mapped for the provider',
        )
      }
      const mapping: MappingRecord = {
        id: freshId(),
        provider: body.provider,
        external_group: group,
        role_id: roleId,
        priority: typeof body.priority === 'number' ? body.priority : 100,
      }
      commit({ mappings: [...identity().mappings, mapping] })
      audit(caller, {
        kind: 'rbac_change',
        action: 'idp_group_mapping.create',
        target: group,
        detail: { provider: body.provider, role: findRole(roleId).name },
      })
      return jsonWithEtag(view(mapping), etagOfMapping(mapping), 201)
    }),
  ),

  http.get('/api/v1/idp-group-mappings/:mappingId', ({ request, params }) =>
    handle(request, { feature: 'mappings' }, () => {
      const mapping = findMapping(String(params.mappingId))
      return jsonWithEtag(view(mapping), etagOfMapping(mapping))
    }),
  ),

  http.put('/api/v1/idp-group-mappings/:mappingId', ({ request, params }) =>
    handle(request, { feature: 'mappings' }, async (caller) => {
      const mapping = findMapping(String(params.mappingId))
      requireIfMatch(request, etagOfMapping(mapping))
      const body = await readJson<{ role_id?: unknown; priority?: unknown }>(request)
      const roleId = checkRole(body.role_id)
      if (typeof body.priority !== 'number') {
        throw invalidInput([
          { field: '/priority', code: 'invalid', message: 'Enter a whole number.' },
        ])
      }
      const next = { ...mapping, role_id: roleId, priority: body.priority }
      commit({ mappings: identity().mappings.map((m) => (m.id === mapping.id ? next : m)) })
      audit(caller, {
        kind: 'rbac_change',
        action: 'idp_group_mapping.update',
        target: mapping.external_group,
        detail: { priority: next.priority },
      })
      return jsonWithEtag(view(next), etagOfMapping(next))
    }),
  ),

  http.delete('/api/v1/idp-group-mappings/:mappingId', ({ request, params }) =>
    handle(request, { feature: 'mappings' }, (caller) => {
      const mapping = findMapping(String(params.mappingId))
      checkOptionalIfMatch(request, etagOfMapping(mapping))
      commit({ mappings: identity().mappings.filter((m) => m.id !== mapping.id) })
      audit(caller, {
        kind: 'rbac_change',
        action: 'idp_group_mapping.delete',
        target: mapping.external_group,
      })
      return noContent()
    }),
  ),
]
