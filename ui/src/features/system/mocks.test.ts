import { beforeEach, describe, expect, it } from 'vitest'
import { api } from '@/api/client'
import { setCsrfToken } from '@/api/csrf'
import { handlers } from '@/mocks/handlers'
import { readSession, startSession } from '@/mocks/session'
import { server } from '@/test/server'
import { resetBlobStoreMock } from '@/features/blob-stores/mocks'
import { resetCleanupMock } from '@/features/cleanup/mocks'
import { resetLifecycleMock } from '@/features/lifecycle/mocks'
import { resetRoutingMock } from '@/features/routing/mocks'
import { resetSettingsMock } from '@/features/settings/mocks'
import { resetTasksMock } from '@/features/tasks/mocks'
import { mockId } from './mock-kit'

/**
 * The browser mock answers the way the contract says, and the page tests, the Playwright specs
 * and the screenshots all lean on it, so its refusals (400, 404, 409, 428, 503) are tested too.
 */
function signIn(user: 'admin' | 'alice'): void {
  server.use(...handlers)
  const session = startSession(user)
  setCsrfToken(session.csrf)
}

async function call(promise: Promise<{ response: Response; error?: unknown }>) {
  const { response, error } = await promise
  const code = (error as { error?: { code?: string } } | undefined)?.error?.code
  return { status: response.status, code }
}

beforeEach(() => {
  resetBlobStoreMock()
  resetCleanupMock()
  resetLifecycleMock()
  resetRoutingMock()
  resetSettingsMock()
  resetTasksMock()
})

describe('administration mock guards', () => {
  it('answers 401 without a session and 403 to a developer', async () => {
    server.use(...handlers)
    expect(await call(api.GET('/api/v1/blob-stores'))).toMatchObject({ status: 401 })
    signIn('alice')
    expect(await call(api.GET('/api/v1/blob-stores'))).toMatchObject({
      status: 403,
      code: 'forbidden',
    })
    expect(readSession()).not.toBeNull()
  })

  it('injects an error from the browser tests, only for the method it names', async () => {
    signIn('admin')
    window.sessionStorage.setItem(
      'ryx-mock-faults',
      JSON.stringify([
        { match: '/tasks', status: 503, code: 'overloaded', retryAfterSecs: 2, method: 'GET' },
      ]),
    )
    expect(await call(api.GET('/api/v1/tasks'))).toMatchObject({ status: 503, code: 'overloaded' })
    expect(await call(api.GET('/api/v1/blob-stores'))).toMatchObject({ status: 200 })
    window.sessionStorage.setItem('ryx-mock-faults', '{not json')
    expect(await call(api.GET('/api/v1/tasks'))).toMatchObject({ status: 200 })
  })

  it('rejects a bad limit and a bad page token', async () => {
    signIn('admin')
    expect(await call(api.GET('/api/v1/tasks', { params: { query: { limit: 0 } } }))).toMatchObject(
      {
        status: 400,
        code: 'invalid_query',
      },
    )
    expect(
      await call(api.GET('/api/v1/tasks', { params: { query: { after: 'garbage' } } })),
    ).toMatchObject({
      status: 400,
      code: 'invalid_page_token',
    })
  })

  it('pages a list with limit and after', async () => {
    signIn('admin')
    const first = await api.GET('/api/v1/tasks', { params: { query: { limit: 2 } } })
    expect(first.data?.items).toHaveLength(2)
    const next = first.data?.next ?? undefined
    expect(next).toBeDefined()
    const second = await api.GET('/api/v1/tasks', { params: { query: { limit: 2, after: next } } })
    expect(second.data?.items[0]?.id).not.toBe(first.data?.items[0]?.id)
  })
})

describe('blob store mock', () => {
  it('refuses to change a location that repositories use, and a missing store', async () => {
    signIn('admin')
    const read = await api.GET('/api/v1/blob-stores/{blob_store_name}', {
      params: { path: { blob_store_name: 'fast-ssd' } },
    })
    const etag = read.response.headers.get('etag') ?? ''
    const patch = (name: string, config: Record<string, unknown>, tag = etag) =>
      api.PATCH('/api/v1/blob-stores/{blob_store_name}', {
        params: { path: { blob_store_name: name }, header: { 'If-Match': tag } },
        headers: { 'Content-Type': 'application/merge-patch+json' },
        body: { config },
      })
    expect(await call(patch('fast-ssd', { path: '/elsewhere' }))).toMatchObject({
      status: 400,
      code: 'immutable_field',
    })
    expect(await call(patch('fast-ssd', { path: '/mnt/ssd/blobs' }))).toMatchObject({ status: 200 })
    expect(await call(patch('nope', {}))).toMatchObject({
      status: 404,
      code: 'blob_store_not_found',
    })
    expect(await call(patch('fast-ssd', {}, '"stale"'))).toMatchObject({ status: 412 })
    expect(
      await call(
        api.DELETE('/api/v1/blob-stores/{blob_store_name}', {
          params: { path: { blob_store_name: 'nope' } },
        }),
      ),
    ).toMatchObject({ status: 404 })
  })

  it('answers the usage of an unreachable store with 503 and Retry-After', async () => {
    signIn('admin')
    const result = await api.GET('/api/v1/blob-stores/{blob_store_name}/usage', {
      params: { path: { blob_store_name: 'cold-s3' } },
    })
    expect(result.response.status).toBe(503)
    expect(result.response.headers.get('retry-after')).toBe('5')
    expect(
      await call(
        api.GET('/api/v1/blob-stores/{blob_store_name}/usage', {
          params: { path: { blob_store_name: 'nope' } },
        }),
      ),
    ).toMatchObject({ status: 404 })
  })

  it('refuses a probe failure and an unknown kind of body when creating', async () => {
    signIn('admin')
    const create = (body: unknown) => api.POST('/api/v1/blob-stores', { body: body as never })
    expect(
      await call(create({ name: 'x', kind: 'local', config: { path: '/nonexistent/x' } })),
    ).toMatchObject({ status: 400, code: 'invalid_input' })
    expect(
      await call(
        create({
          name: 'y',
          kind: 's3',
          config: {
            endpoint: 'https://unreachable.example.org',
            bucket: 'b',
            region: 'r',
            key_prefix: '',
            path_style: true,
            part_size_mb: 16,
          },
        }),
      ),
    ).toMatchObject({ status: 400, code: 'invalid_input' })
    expect(await call(create({ name: 'z', kind: 'tape' }))).toMatchObject({
      status: 400,
      code: 'invalid_json',
    })
    const refs = await api.POST('/api/v1/blob-stores', {
      body: {
        name: 'with-refs',
        kind: 's3',
        config: {
          endpoint: 'https://minio.example.org',
          bucket: 'b',
          region: 'r',
          key_prefix: '',
          path_style: true,
          part_size_mb: 16,
        },
        credentials: { access_key_ref: 'env:A', secret_key_ref: 'env:B' },
      },
    })
    expect(refs.data?.credentials).toMatchObject({ source: 'reference', access_key_ref: 'env:A' })
  })
})

describe('cleanup policy mock', () => {
  const policy = mockId(2, 2)
  const criteria = (over: Record<string, unknown>) => ({
    older_than_days: null,
    last_downloaded_days: null,
    keep_last_versions: null,
    name_pattern: null,
    release_type: null,
    ...over,
  })

  it('refuses criteria the format does not support, an empty policy and a changed format in use', async () => {
    signIn('admin')
    const create = (format: string, c: Record<string, unknown>) =>
      api.POST('/api/v1/cleanup-policies', {
        body: {
          name: `p-${Math.random()}`,
          description: '',
          format: format as 'raw',
          criteria: criteria(c) as never,
        },
      })
    expect(await call(create('raw', { release_type: 'release' }))).toMatchObject({
      status: 400,
      code: 'invalid_input',
    })
    expect(await call(create('raw', {}))).toMatchObject({ status: 400, code: 'invalid_input' })
    const read = await api.GET('/api/v1/cleanup-policies/{policy_id}', {
      params: { path: { policy_id: policy } },
    })
    const put = (body: unknown, tag?: string) =>
      api.PUT('/api/v1/cleanup-policies/{policy_id}', {
        params: {
          path: { policy_id: policy },
          header: { 'If-Match': tag ?? read.response.headers.get('etag') ?? '' },
        },
        body: body as never,
      })
    expect(
      await call(
        put({
          name: 'maven-old-snapshots',
          description: '',
          format: 'npm',
          criteria: criteria({ older_than_days: 3 }),
        }),
      ),
    ).toMatchObject({ status: 409, code: 'already_exists' })
    expect(
      await call(
        put({
          name: 'renamed',
          description: '',
          format: 'docker',
          criteria: criteria({ older_than_days: 3 }),
        }),
      ),
    ).toMatchObject({ status: 400, code: 'immutable_field' })
    expect(await call(put({ name: 'x' }, '"stale"'))).toMatchObject({ status: 412 })
  })

  it('answers 404 for unknown policies and refuses a delete in use without force', async () => {
    signIn('admin')
    const unknown = { path: { policy_id: mockId(2, 999) } }
    expect(
      await call(api.GET('/api/v1/cleanup-policies/{policy_id}', { params: unknown })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(api.DELETE('/api/v1/cleanup-policies/{policy_id}', { params: unknown })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.POST('/api/v1/cleanup-policies/{policy_id}/actions/preview', { params: unknown }),
      ),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.DELETE('/api/v1/cleanup-policies/{policy_id}', {
          params: { path: { policy_id: policy } },
        }),
      ),
    ).toMatchObject({ status: 409, code: 'resource_in_use' })
  })
})

describe('routing rule mock', () => {
  const rule = mockId(9, 1)

  it('lints, tests and refuses like the engine does', async () => {
    signIn('admin')
    const invalid = await api.POST('/api/v1/routing-rules/validate', {
      body: { patterns: ['', '(', '(?=x)', '^/ok'] },
    })
    expect(invalid.data?.errors.map((e) => e.index)).toEqual([0, 1, 2])
    const many = Array.from({ length: 101 }, (_, i) => `/p${i}`)
    expect(
      await call(
        api.POST('/api/v1/routing-rules/test', {
          body: { mode: 'block', patterns: ['^/a'], paths: many },
        }),
      ),
    ).toMatchObject({ status: 400, code: 'invalid_query' })
    expect(
      await call(
        api.POST('/api/v1/routing-rules/test', {
          body: { mode: 'block', patterns: ['(?=x)'], paths: ['/a'] },
        }),
      ),
    ).toMatchObject({ status: 400, code: 'invalid_pattern' })
    const saved = await api.POST('/api/v1/routing-rules/{rule_id}/test', {
      params: { path: { rule_id: rule } },
      body: { paths: ['/internal/a', '/other'] },
    })
    expect(saved.data?.items.map((i) => i.outcome)).toEqual(['blocked', 'allowed'])
  })

  it('refuses a duplicate name, an invalid pattern, a stale write and an unknown rule', async () => {
    signIn('admin')
    expect(
      await call(
        api.POST('/api/v1/routing-rules', {
          body: { name: 'block-executables', description: '', mode: 'block', patterns: ['^/a'] },
        }),
      ),
    ).toMatchObject({ status: 409, code: 'already_exists' })
    expect(
      await call(
        api.POST('/api/v1/routing-rules', {
          body: { name: 'fresh', description: '', mode: 'block', patterns: ['(?!x)'] },
        }),
      ),
    ).toMatchObject({ status: 400, code: 'invalid_pattern' })
    const read = await api.GET('/api/v1/routing-rules/{rule_id}', {
      params: { path: { rule_id: rule } },
    })
    const put = (body: unknown, tag: string) =>
      api.PUT('/api/v1/routing-rules/{rule_id}', {
        params: { path: { rule_id: rule }, header: { 'If-Match': tag } },
        body: body as never,
      })
    const etag = read.response.headers.get('etag') ?? ''
    expect(
      await call(
        put({ name: 'block-executables', description: '', mode: 'block', patterns: ['^/a'] }, etag),
      ),
    ).toMatchObject({ status: 409 })
    expect(
      await call(put({ name: 'x', description: '', mode: 'block', patterns: ['('] }, etag)),
    ).toMatchObject({ status: 400 })
    expect(await call(put({}, '"stale"'))).toMatchObject({ status: 412 })
    const missing = { path: { rule_id: mockId(9, 999) } }
    expect(
      await call(api.GET('/api/v1/routing-rules/{rule_id}', { params: missing })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(api.DELETE('/api/v1/routing-rules/{rule_id}', { params: missing })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.POST('/api/v1/routing-rules/{rule_id}/test', { params: missing, body: { paths: [] } }),
      ),
    ).toMatchObject({ status: 404 })
  })
})

describe('settings mock', () => {
  it('refuses unknown sections, fields from the configuration and out of range values', async () => {
    signIn('admin')
    expect(
      await call(
        api.GET('/api/v1/settings/{section}', {
          params: { path: { section: 'nope' as 'general' } },
        }),
      ),
    ).toMatchObject({ status: 404 })
    const read = await api.GET('/api/v1/settings/{section}', {
      params: { path: { section: 'security' } },
    })
    const etag = read.response.headers.get('etag') ?? ''
    const put = (section: 'security' | 'general', values: Record<string, unknown>, tag = etag) =>
      api.PUT('/api/v1/settings/{section}', {
        params: { path: { section }, header: { 'If-Match': tag } },
        body: { values },
      })
    expect(await call(put('security', { session_idle_timeout_secs: 5 }))).toMatchObject({
      status: 409,
      code: 'setting_managed_by_config',
    })
    expect(await call(put('security', { token_max_ttl_days: 0 }))).toMatchObject({
      status: 400,
      code: 'invalid_input',
    })
    expect(await call(put('security', { token_max_ttl_days: 30 }, '"stale"'))).toMatchObject({
      status: 412,
    })
    expect(await call(put('security', { token_default_ttl_days: 30 }))).toMatchObject({
      status: 200,
    })
  })
})

describe('task and lifecycle mock', () => {
  it('answers 404 for unknown tasks and runs and pages a log window', async () => {
    signIn('admin')
    const task = mockId(7, 1)
    expect(
      await call(api.GET('/api/v1/tasks/{task_id}', { params: { path: { task_id: 'nope' } } })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.GET('/api/v1/tasks/{task_id}/runs', { params: { path: { task_id: 'nope' } } }),
      ),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.GET('/api/v1/tasks/{task_id}/runs/{run_id}/log', {
          params: { path: { task_id: 'nope', run_id: 'x' } },
        }),
      ),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.GET('/api/v1/tasks/{task_id}/runs/{run_id}/log', {
          params: { path: { task_id: task, run_id: 'x' } },
        }),
      ),
    ).toMatchObject({ status: 404, code: 'task_run_not_found' })
    const run = mockId(8, 100)
    const window = await api.GET('/api/v1/tasks/{task_id}/runs/{run_id}/log', {
      params: { path: { task_id: task, run_id: run }, query: { from_line: 5, limit: 3 } },
    })
    expect(window.data?.lines.map((l) => l.n)).toEqual([5, 6, 7])
    expect(window.data?.next_line).toBe(8)
  })

  it('answers not ready while a run plans, and 404 for unknown runs', async () => {
    signIn('admin')
    const running = mockId(3, 4)
    expect(
      await call(
        api.GET('/api/v1/lifecycle-runs/{run_id}/candidates', {
          params: { path: { run_id: running } },
        }),
      ),
    ).toMatchObject({ status: 409, code: 'not_ready' })
    expect(
      await call(
        api.GET('/api/v1/lifecycle-runs/{run_id}/candidates', {
          params: { path: { run_id: 'nope' } },
        }),
      ),
    ).toMatchObject({ status: 404 })
    expect(
      await call(api.GET('/api/v1/gc/runs/{run_id}', { params: { path: { run_id: 'nope' } } })),
    ).toMatchObject({ status: 404 })
    expect(
      await call(
        api.GET('/api/v1/gc/runs/{run_id}/candidates', { params: { path: { run_id: 'nope' } } }),
      ),
    ).toMatchObject({ status: 404 })
    const filtered = await api.GET('/api/v1/gc/runs', {
      params: { query: { dry_run: true, status: 'succeeded' } },
    })
    expect(filtered.data?.items.every((r) => r.dry_run)).toBe(true)
  })
})
