import { delay, http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { MOCK_USERS } from '@/mocks/data'
import { readSession, userOf } from '@/mocks/session'
import { json, LATENCY_MS, minutesAgo, visibleRepositories } from '@/mocks/store'

type RepositoryView = components['schemas']['RepositoryView']

/**
 * Mock endpoints of this feature. The shell's mock (src/mocks/handlers.ts) discovers every
 * mocks.ts file below src/features by itself: a feature that needs a stateful or computed answer instead
 * of the generated OpenAPI example writes its handlers here and exports `handlers`.
 */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/dashboard/summary', async () => {
    await delay(LATENCY_MS)
    const session = readSession()
    const user = userOf(session)
    const repos = visibleRepositories()
    const count = (pick: (r: RepositoryView) => string): Record<string, number> =>
      repos.reduce<Record<string, number>>(
        (acc, r) => ({ ...acc, [pick(r)]: (acc[pick(r)] ?? 0) + 1 }),
        {},
      )
    const summary = {
      public_only: user === null,
      repositories: {
        total: repos.length,
        offline: repos.filter((r) => !r.online).length,
        by_format: count((r) => r.format),
        by_type: count((r) => r.repo_type),
      },
      storage: null,
      recent_activity: null,
      failed_tasks: null,
      setup_checklist: null,
      degraded: null,
    }
    if (user?.admin !== true) return json(summary)
    return json({
      ...summary,
      storage: [
        {
          blob_store: 'default',
          used_bytes: 412_316_860_416,
          quota_bytes: 1_099_511_627_776,
          health: 'ok',
        },
        {
          blob_store: 'archive-s3',
          used_bytes: 8_123_456_789,
          quota_bytes: null,
          health: 'degraded',
        },
      ],
      recent_activity: [
        {
          id: '0192f6c4-5a1e-7000-8000-0000000000a1',
          at: minutesAgo(6),
          kind: 'rbac_change',
          actor_id: MOCK_USERS[0]?.id ?? null,
          actor_name: 'admin',
          client_ip: '10.1.2.3',
          request_id: '0192f6c4-5a1e-7000-8000-00000000c0de',
          action: 'role.update',
          target: 'maven-publishers',
          outcome: 'success',
          detail: {},
        },
        {
          id: '0192f6c4-5a1e-7000-8000-0000000000a2',
          at: minutesAgo(41),
          kind: 'login_failure',
          actor_id: null,
          actor_name: 'bob',
          client_ip: '10.1.2.4',
          request_id: '0192f6c4-5a1e-7000-8000-00000000c0df',
          action: 'auth.login',
          target: null,
          outcome: 'failure',
          detail: {},
        },
      ],
      failed_tasks: [],
      setup_checklist: [
        { id: 'create_blob_store', done: true, link: '/admin/blob-stores' },
        { id: 'create_repository', done: true, link: '/repositories/new' },
        { id: 'set_base_url', done: true, link: '/admin/settings' },
        { id: 'create_token', done: false, link: '/me/tokens' },
      ],
      degraded: false,
    })
  }),
]
