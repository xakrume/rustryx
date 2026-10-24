import { http, HttpResponse, type HttpHandler } from 'msw'
import {
  handle,
  invalidInput,
  jsonWithEtag,
  MockProblem,
  readJson,
  requireIfMatch,
} from '@/features/roles/mock-http'
import { ROLE_ID } from '@/features/roles/mock-seed'
import { audit, commit, etagOfAnonymous, identity } from '@/features/roles/mock-store'
import { repositories } from '@/mocks/store'

function stateBody() {
  const { anonymous } = identity()
  const flagged = repositories.filter((r) => r.allow_anonymous).length
  return {
    global_enabled: anonymous.globalEnabled,
    global_editable: anonymous.editable,
    anonymous_role: { id: ROLE_ID.anonymous, name: 'nx-anonymous' },
    public_repository_count: flagged,
    effective_repository_count: anonymous.globalEnabled ? flagged : 0,
  }
}

/**
 * Stateful mock of anonymous access. The global switch is read only by default (as in the first
 * version of the contract); a repository flag changes the shared repository list of the mock, so
 * the repository pages see it too.
 */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/anonymous-access', ({ request }) =>
    handle(request, { feature: 'anonymous' }, () => jsonWithEtag(stateBody(), etagOfAnonymous())),
  ),

  http.put('/api/v1/anonymous-access', ({ request }) =>
    handle(request, { feature: 'anonymous' }, async (caller) => {
      requireIfMatch(request, etagOfAnonymous())
      const body = await readJson<{ global_enabled?: unknown }>(request)
      if (typeof body.global_enabled !== 'boolean') {
        throw invalidInput([
          { field: '/global_enabled', code: 'invalid', message: 'Send true or false.' },
        ])
      }
      if (!identity().anonymous.editable) {
        throw new MockProblem(
          409,
          'setting_managed_by_config',
          'The value comes from the configuration and cannot change at runtime',
        )
      }
      commit({ anonymous: { ...identity().anonymous, globalEnabled: body.global_enabled } })
      for (const [index, repo] of repositories.entries()) {
        repositories[index] = {
          ...repo,
          anonymous_effective: repo.allow_anonymous && body.global_enabled,
        }
      }
      audit(caller, {
        kind: 'config_change',
        action: 'anonymous_access.update',
        target: null,
        detail: { global_enabled: body.global_enabled },
      })
      return jsonWithEtag(stateBody(), etagOfAnonymous())
    }),
  ),

  http.put('/api/v1/anonymous-access/repositories', ({ request }) =>
    handle(request, { feature: 'anonymous' }, async (caller) => {
      const body = await readJson<{
        repositories?: { name?: string; allow_anonymous?: boolean }[]
      }>(request)
      const changes = body.repositories ?? []
      if (changes.length < 1 || changes.length > 500) {
        throw invalidInput([
          {
            field: '/repositories',
            code: 'invalid',
            message: 'Send between 1 and 500 repositories.',
          },
        ])
      }
      // All or nothing: the first unknown name is a 404 and nothing was changed.
      const unknown = changes.find((c) => !repositories.some((r) => r.name === c.name))
      if (unknown) throw new MockProblem(404, 'repository_not_found', 'Repository not found')
      const global = identity().anonymous.globalEnabled
      let updated = 0
      for (const change of changes) {
        const index = repositories.findIndex((r) => r.name === change.name)
        const repo = repositories[index]
        if (repo && repo.allow_anonymous !== change.allow_anonymous) {
          repositories[index] = {
            ...repo,
            allow_anonymous: change.allow_anonymous === true,
            anonymous_effective: change.allow_anonymous === true && global,
          }
          updated += 1
        }
      }
      audit(caller, {
        kind: 'config_change',
        action: 'anonymous_access.repositories',
        target: null,
        detail: { updated },
      })
      return HttpResponse.json({ updated })
    }),
  ),
]
