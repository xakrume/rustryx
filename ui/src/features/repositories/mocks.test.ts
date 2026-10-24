import { Blob as NodeBlob, File as NodeFile } from 'node:buffer'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { handlers } from '@/mocks/handlers'
import { startSession } from '@/mocks/session'
import { repositories } from '@/mocks/store'
import { emptyJson, nth, parseJson, type ApiJson } from '@/test/api-json'
import { server } from '@/test/server'
import { MOCK_FORMATS, mockUploadSchema } from './mock-formats'
import { paginate } from './mock-page'
import { resetRepositories } from './mock-state'

/**
 * The repository mock is the contract the pages are built against, so it is tested like one:
 * every rule of docs/api-conventions.md it claims to keep is exercised here through plain HTTP.
 */
const ORIGIN = 'http://localhost:3000'
type Who = 'admin' | 'alice' | null

async function call(
  method: string,
  path: string,
  body?: unknown,
  who: Who = 'admin',
  headers: Record<string, string> = {},
) {
  // An anonymous call has no session at all, including none left over from an earlier call.
  if (who === null) window.sessionStorage.clear()
  const session = who === null ? null : startSession(who)
  const response = await fetch(`${ORIGIN}${path}`, {
    method,
    headers: {
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...(session ? { 'X-CSRF-Token': session.csrf } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await response.text()
  return {
    status: response.status,
    headers: response.headers,
    body: text ? parseJson(text) : emptyJson(),
  }
}

const BLOB = '0192f6c4-5a1e-7000-8000-000000000029'
const hosted = (name: string, extra: Record<string, unknown> = {}) => ({
  name,
  format: 'maven2',
  repo_type: 'hosted',
  blob_store_id: BLOB,
  ...extra,
})

beforeEach(() => {
  // The test setup resets the handlers after every test, so they are registered again each time.
  server.use(...handlers)
  resetRepositories()
  resetContent()
})

describe('repository list and detail', () => {
  it('filters by every parameter and sorts by name or creation', async () => {
    const q = async (query: string): Promise<ApiJson[]> =>
      (await call('GET', `/api/v1/repositories?${query}`, undefined, 'admin')).body.items
    expect((await q('format=npm&repo_type=proxy')).map((r) => r.name)).toEqual(['npm-proxy'])
    expect((await q('online=false')).every((r) => r.online === false)).toBe(true)
    expect((await q('blob_store=default&limit=3')).length).toBe(3)
    expect((await q('allow_anonymous=true&q=maven')).every((r) => r.allow_anonymous)).toBe(true)
    const desc = await q('sort=-name&limit=2')
    expect(nth(desc, 0).name > nth(desc, 1).name).toBe(true)
    expect((await q('sort=created_at&limit=2')).length).toBe(2)
    expect((await q('limit=2')).every((r) => r.stats === null)).toBe(true)
    expect((await q('limit=2&include=stats')).every((r) => r.stats !== null)).toBe(true)
  })

  it('refuses a bad sort, a bad limit and a page token from elsewhere', async () => {
    expect((await call('GET', '/api/v1/repositories?sort=size')).body.error.code).toBe(
      'unsupported_sort',
    )
    expect((await call('GET', '/api/v1/repositories?limit=0')).body.error.code).toBe(
      'invalid_query',
    )
    expect((await call('GET', '/api/v1/repositories?after=%25%25')).body.error.code).toBe(
      'invalid_page_token',
    )
  })

  it('answers the trigger searches with 503 and 429 and Retry-After', async () => {
    const unavailable = await call('GET', '/api/v1/repositories?q=error503')
    expect(unavailable.status).toBe(503)
    expect(unavailable.headers.get('Retry-After')).toBe('2')
    expect((await call('GET', '/api/v1/repositories?q=error429')).status).toBe(429)
  })

  it('hides a private repository from an anonymous caller as not found, never as forbidden', async () => {
    const hidden = await call('GET', '/api/v1/repositories/maven-releases', undefined, null)
    expect(hidden.status).toBe(404)
    expect(hidden.body.error.code).toBe('repository_not_found')
    expect(
      (await call('GET', '/api/v1/repositories/maven-proxy', undefined, null)).headers.get('ETag'),
    ).toMatch(/^"repo-/)
  })

  it('computes the permissions of the caller on each repository', async () => {
    const as = async (who: Who, name: string) =>
      (await call('GET', `/api/v1/repositories/${name}`, undefined, who)).body.permissions
    expect(await as('admin', 'maven-releases')).toMatchObject({
      write: true,
      delete: true,
      admin: true,
    })
    expect(await as('alice', 'maven-releases')).toMatchObject({
      write: true,
      delete: false,
      admin: false,
    })
    expect(await as('alice', 'maven-group')).toMatchObject({ write: false, delete: false })
    expect(await as(null, 'maven-proxy')).toMatchObject({ write: false, read: true })
  })
})

describe('creating a repository', () => {
  it('creates, answers 201 with Location and ETag, and refuses a taken name', async () => {
    const created = await call(
      'POST',
      '/api/v1/repositories',
      hosted('fresh', {
        allow_anonymous: true,
        quota_bytes: 5,
        cleanup_policy_ids: ['0192f6c4-5a1e-7000-8000-00000000006f'],
        routing_rule_id: '0192f6c4-5a1e-7000-8000-000000000079',
      }),
    )
    expect(created.status).toBe(201)
    expect(created.headers.get('Location')).toBe('/api/v1/repositories/fresh')
    expect(created.body).toMatchObject({
      write_policy: 'allow_once',
      anonymous_effective: true,
      quota_bytes: 5,
    })
    expect(nth(created.body.cleanup_policies, 0).name).toBe('maven-old-snapshots')
    expect(created.body.routing_rule.name).toBe('block-snapshots')
    expect((await call('POST', '/api/v1/repositories', hosted('fresh'))).body.error.code).toBe(
      'already_exists',
    )
  })

  it('answers field errors with JSON pointers', async () => {
    const check = async (body: unknown): Promise<string[]> =>
      (await call('POST', '/api/v1/repositories', body)).body.error.details.fields.map(
        (f) => f.field,
      )
    expect(await check(hosted('bad name'))).toEqual(['/name'])
    expect(await check(hosted('x', { format: 'cobol' }))).toContain('/format')
    expect(
      await check(hosted('x', { blob_store_id: '00000000-0000-0000-0000-000000000000' })),
    ).toEqual(['/blob_store_id'])
    expect(await check(hosted('x', { proxy: { remote_url: 'https://a.org' } }))).toEqual(['/proxy'])
    expect(await check(hosted('x', { repo_type: 'proxy' }))).toEqual(['/proxy'])
    expect(
      await check(
        hosted('x', {
          repo_type: 'group',
          group: { members: ['maven-releases', 'npm-releases', 'nope'] },
        }),
      ),
    ).toEqual(['/group/members/1', '/group/members/2'])
    expect(await check(hosted('x', { repo_type: 'proxy', write_policy: 'allow' }))).toEqual([
      '/write_policy',
      '/proxy',
    ])
  })

  it('refuses a type the format does not support', async () => {
    const cobol = MOCK_FORMATS.find((f) => f.format === 'maven2')!
    const original = cobol.supports.group
    ;(cobol.supports as { group: boolean }).group = false
    try {
      const result = await call(
        'POST',
        '/api/v1/repositories',
        hosted('g', { repo_type: 'group', group: { members: [] } }),
      )
      expect(nth(result.body.error.details.fields, 0).code).toBe('unsupported_type')
    } finally {
      ;(cobol.supports as { group: boolean }).group = original
    }
  })

  it('applies the SSRF filter with the rule named, and reports odd URLs as field errors', async () => {
    const proxy = (remote_url: string) => hosted('p', { repo_type: 'proxy', proxy: { remote_url } })
    const rejected = await call('POST', '/api/v1/repositories', proxy('http://10.0.0.5/x'))
    expect(rejected.body.error).toMatchObject({
      code: 'upstream_url_rejected',
      details: { rule: 'private_address' },
    })
    expect(
      nth(
        (await call('POST', '/api/v1/repositories', proxy('ftp://example.org'))).body.error.details
          .fields,
        0,
      ).code,
    ).toBe('invalid_scheme')
    expect(
      nth(
        (await call('POST', '/api/v1/repositories', proxy('not a url'))).body.error.details.fields,
        0,
      ).code,
    ).toBe('invalid_url')
    const ok = await call('POST', '/api/v1/repositories', proxy('https://registry.example.org'))
    expect(ok.status).toBe(201)
    expect(ok.body.proxy).toMatchObject({
      content_max_age_secs: 86400,
      max_concurrent_upstream: 16,
      auth: null,
    })
    expect(ok.body.upstream.state).toBe('unknown')
  })

  it('answers 403 to a developer, 401 without a session, 400 for a broken body, and the rate and outage triggers', async () => {
    expect((await call('POST', '/api/v1/repositories', hosted('a'), 'alice')).body.error.code).toBe(
      'forbidden',
    )
    expect((await call('POST', '/api/v1/repositories', hosted('a'), null)).status).toBe(401)
    const broken = await fetch(`${ORIGIN}/api/v1/repositories`, {
      method: 'POST',
      headers: { 'X-CSRF-Token': startSession('admin').csrf },
      body: '{',
    })
    expect(broken.status).toBe(400)
    expect((await call('POST', '/api/v1/repositories', hosted('ratelimited'))).status).toBe(429)
    expect((await call('POST', '/api/v1/repositories', hosted('unavailable'))).status).toBe(503)
  })

  it('keeps a password out of the answer', async () => {
    const result = await call(
      'POST',
      '/api/v1/repositories',
      hosted('secret-check', {
        repo_type: 'proxy',
        proxy: {
          remote_url: 'https://r.example.org',
          auth: { username: 'u', password: 'a very secret value' },
        },
      }),
    )
    expect(result.body.proxy.auth).toEqual({ username: 'u', password_set: true })
    expect(JSON.stringify(result.body)).not.toContain('a very secret value')
  })
})

describe('changing and deleting', () => {
  async function etagOf(name: string): Promise<string> {
    return (await call('GET', `/api/v1/repositories/${name}`)).headers.get('ETag')!
  }
  const patch = async (name: string, body: unknown, etag?: string, who: Who = 'admin') =>
    call(
      'PATCH',
      `/api/v1/repositories/${name}`,
      body,
      who,
      etag === undefined ? {} : { 'If-Match': etag },
    )

  it('needs If-Match and answers 428 without it and 412 with a stale one', async () => {
    expect((await patch('npm-releases', { online: false })).status).toBe(428)
    expect((await patch('npm-releases', { online: false }, '"stale"')).status).toBe(412)
    const etag = await etagOf('npm-releases')
    const first = await patch('npm-releases', { online: false }, etag)
    expect(first.status).toBe(200)
    expect(first.headers.get('ETag')).not.toBe(etag)
    // The same version again is stale now.
    expect((await patch('npm-releases', { online: true }, etag)).status).toBe(412)
  })

  it('merges proxy settings, credentials and format options as a merge patch', async () => {
    const etag = await etagOf('npm-proxy')
    const result = await patch(
      'npm-proxy',
      {
        write_policy: 'allow',
        allow_anonymous: false,
        quota_bytes: null,
        cleanup_policy_ids: ['0192f6c4-5a1e-7000-8000-00000000006f'],
        routing_rule_id: '0192f6c4-5a1e-7000-8000-000000000079',
        proxy: { content_max_age_secs: 5, auth: { username: 'x', password: 'y' } },
        format_config: { strict: true, gone: null },
      },
      etag,
    )
    expect(result.body.proxy).toMatchObject({
      content_max_age_secs: 5,
      metadata_max_age_secs: 1800,
      auth: { username: 'x', password_set: true },
    })
    expect(result.body.format_config).toEqual({ strict: true })
    expect(result.body.routing_rule.name).toBe('block-snapshots')
    const again = await patch(
      'npm-proxy',
      { routing_rule_id: null, proxy: { auth: null } },
      result.headers.get('ETag')!,
    )
    expect(again.body.routing_rule).toBeNull()
    expect(again.body.proxy.auth).toBeNull()
    const keep = await patch(
      'npm-proxy',
      { proxy: { auth: { username: 'z' } } },
      again.headers.get('ETag')!,
    )
    expect(keep.body.proxy.auth).toEqual({ username: 'z', password_set: false })
  })

  it('refuses to change a field that is fixed at creation, with the same value accepted', async () => {
    const etag = await etagOf('npm-releases')
    const refused = await patch('npm-releases', { name: 'other' }, etag)
    expect(refused.body.error).toMatchObject({ code: 'immutable_field' })
    expect((await patch('npm-releases', { name: 'npm-releases' }, etag)).status).toBe(200)
  })

  it('validates members of a group and refuses a cycle with a 409 that names the loop', async () => {
    const etag = await etagOf('npm-group')
    expect(
      nth(
        (await patch('npm-group', { group: { members: ['maven-releases'] } }, etag)).body.error
          .details.fields,
        0,
      ).code,
    ).toBe('format_mismatch')
    expect(
      nth(
        (await patch('npm-group', { group: { members: ['npm-group'] } }, etag)).body.error.details
          .fields,
        0,
      ).code,
    ).toBe('self_reference')
    const cycle = await patch('npm-group', { group: { members: ['npm-group-b'] } }, etag)
    expect(nth(cycle.body.error.details.fields, 0).code).toBe('unknown_repository')
    const inner = repositories.find((r) => r.name === 'npm-group')!
    repositories.push({
      ...inner,
      id: '0192f6c4-5a1e-7000-8000-0000000fffff',
      name: 'npm-group-b',
      group: { members: ['npm-group'] },
    })
    expect(
      (await patch('npm-group', { group: { members: ['npm-group-b'] } }, etag)).body.error.code,
    ).toBe('conflict')
  })

  it('refuses a bad remote URL on a patch and an unknown repository, and a developer', async () => {
    const etag = await etagOf('npm-proxy')
    expect(
      (await patch('npm-proxy', { proxy: { remote_url: 'http://localhost' } }, etag)).body.error
        .code,
    ).toBe('upstream_url_rejected')
    expect((await patch('nope', { online: true }, '"x"')).status).toBe(404)
    expect((await patch('npm-releases', { online: true }, etag, 'alice')).status).toBe(403)
    const current = await etagOf('npm-releases')
    const broken = await fetch(`${ORIGIN}/api/v1/repositories/npm-releases`, {
      method: 'PATCH',
      headers: { 'X-CSRF-Token': startSession('admin').csrf, 'If-Match': current },
      body: '{',
    })
    expect(broken.status).toBe(400)
  })

  it('deletes in the stages the contract names', async () => {
    const inGroup = await call('DELETE', '/api/v1/repositories/maven-releases')
    expect(inGroup.body.error).toMatchObject({
      code: 'resource_in_use',
      details: { dependents: [{ name: 'maven-group' }] },
    })
    const full = await call('DELETE', '/api/v1/repositories/team-002-pypi')
    expect(full.body.error.code).toBe('repository_not_empty')
    expect(full.body.error.details.component_count).toBeGreaterThan(0)
    expect(
      (await call('DELETE', '/api/v1/repositories/team-002-pypi?include_content=true')).status,
    ).toBe(204)
    expect((await call('DELETE', '/api/v1/repositories/team-002-pypi')).status).toBe(404)
    expect((await call('DELETE', '/api/v1/repositories/team-005-raw')).status).toBe(204)
    expect(
      (await call('DELETE', '/api/v1/repositories/maven-group', undefined, 'alice')).status,
    ).toBe(403)
  })
})

describe('upstream actions and formats', () => {
  it('probes an upstream and reports a failure as an answer, not as an error', async () => {
    const probe = async (remote_url: string) =>
      (await call('POST', '/api/v1/upstream-checks', { format: 'npm', remote_url })).body
    expect(await probe('https://registry.example.org')).toMatchObject({ ok: true, latency_ms: 143 })
    expect(await probe('https://registry.down.example.org')).toMatchObject({
      ok: false,
      error_code: 'upstream_timeout',
    })
    expect(await probe('http://localhost')).toMatchObject({ ok: false, rule: 'private_address' })
    expect(
      (await call('POST', '/api/v1/upstream-checks', { format: 'npm', remote_url: 'nope' })).status,
    ).toBe(400)
    expect(
      (
        await call(
          'POST',
          '/api/v1/upstream-checks',
          { format: 'npm', remote_url: 'https://a.org' },
          'alice',
        )
      ).status,
    ).toBe(403)
  })

  it('invalidates a proxy cache and blocks and unblocks its upstream, and refuses both elsewhere', async () => {
    expect(
      (await call('POST', '/api/v1/repositories/npm-proxy/actions/invalidate-cache')).body
        .invalidated_at,
    ).toBeTruthy()
    expect(
      (await call('POST', '/api/v1/repositories/npm-releases/actions/invalidate-cache')).status,
    ).toBe(409)
    expect((await call('POST', '/api/v1/repositories/nope/actions/invalidate-cache')).status).toBe(
      404,
    )
    const blocked = await call('POST', '/api/v1/repositories/npm-proxy/actions/set-blocked', {
      blocked: true,
    })
    expect(blocked.body.upstream.state).toBe('blocked')
    expect(
      (await call('POST', '/api/v1/repositories/npm-proxy/actions/set-blocked', { blocked: false }))
        .body.upstream.state,
    ).toBe('ok')
    expect(
      (
        await call('POST', '/api/v1/repositories/npm-releases/actions/set-blocked', {
          blocked: true,
        })
      ).status,
    ).toBe(409)
    expect(
      (
        await call(
          'POST',
          '/api/v1/repositories/npm-proxy/actions/set-blocked',
          { blocked: true },
          'alice',
        )
      ).status,
    ).toBe(403)
    expect(
      (await call('POST', '/api/v1/repositories/nope/actions/set-blocked', { blocked: true }))
        .status,
    ).toBe(404)
  })

  it('serves the formats and the upload schemas, and 404 for an unknown format', async () => {
    expect((await call('GET', '/api/v1/formats')).body.items).toHaveLength(MOCK_FORMATS.length)
    expect((await call('GET', '/api/v1/formats/maven2/upload-schema')).body.uploadable).toBe(true)
    expect((await call('GET', '/api/v1/formats/docker/upload-schema')).body.uploadable).toBe(false)
    expect((await call('GET', '/api/v1/formats/cobol/upload-schema')).body.error.code).toBe(
      'format_not_found',
    )
    expect(
      (await call('GET', '/api/v1/formats/maven2/upload-schema', undefined, null)).status,
    ).toBe(401)
    for (const info of MOCK_FORMATS) expect(mockUploadSchema(info.format)?.format).toBe(info.format)
  })

  it('pages with the contract rules', () => {
    const rows = Array.from({ length: 5 }, (_, i) => i)
    const first = paginate(rows, new URL('http://x/y?limit=2')) as { items: number[]; next: string }
    expect(first.items).toEqual([0, 1])
    const second = paginate(rows, new URL(`http://x/y?limit=2&after=${first.next}`)) as {
      items: number[]
      next: string | null
    }
    expect(second.items).toEqual([2, 3])
    expect(paginate(rows, new URL('http://x/y?limit=1000'), { limit: 3, max: 4 })).toMatchObject({
      items: [0, 1, 2, 3],
    })
  })
})

describe('content upload through the mock', () => {
  beforeAll(async () => {
    const nodeFormData = (
      await new Response('', {
        headers: { 'content-type': 'application/x-www-form-urlencoded' },
      }).formData()
    ).constructor
    vi.stubGlobal('FormData', nodeFormData)
    vi.stubGlobal('Blob', NodeBlob)
    vi.stubGlobal('File', NodeFile)
  })
  afterAll(() => vi.unstubAllGlobals())

  async function upload(
    repository: string,
    files: string[],
    fields: Record<string, unknown> = {},
    who: Who = 'admin',
    key?: string,
  ) {
    const form = new FormData()
    form.append(
      'attributes',
      new Blob(
        [JSON.stringify({ fields, assets: files.map(() => ({ fields: {}, sha256: null })) })],
        { type: 'application/json' },
      ),
      'attributes.json',
    )
    for (const name of files) form.append('file', new File(['0123456789'], name), name)
    if (who === null) window.sessionStorage.clear()
    const session = who === null ? null : startSession(who)
    const response = await fetch(`${ORIGIN}/api/v1/components?repository=${repository}`, {
      method: 'POST',
      headers: {
        ...(session ? { 'X-CSRF-Token': session.csrf } : {}),
        ...(key ? { 'Idempotency-Key': key } : {}),
      },
      body: form,
    })
    const text = await response.text()
    return {
      status: response.status,
      headers: response.headers,
      body: text ? parseJson(text) : emptyJson(),
    }
  }

  it('publishes for each format with the path the format uses', async () => {
    const paths = async (
      repository: string,
      files: string[],
      fields: Record<string, unknown> = {},
    ): Promise<string[]> => (await upload(repository, files, fields)).body.assets.map((a) => a.path)
    expect(
      await paths('maven-releases', ['lib-1.0.jar'], {
        group_id: 'org.x',
        artifact_id: 'lib',
        version: '1.0',
      }),
    ).toEqual(['org/x/lib/1.0/lib-1.0.jar'])
    expect(await paths('npm-releases', ['ui-2.0.0.tgz'])).toEqual(['ui/-/ui-2.0.0.tgz'])
    expect(await paths('pypi-releases', ['acme-1.0.0.tar.gz'])).toEqual([
      'packages/acme/1.0.0/acme-1.0.0.tar.gz',
    ])
    expect(await paths('helm-releases', ['chart-1.0.0.tgz'])).toEqual(['charts/chart-1.0.0.tgz'])
    expect(await paths('pub-releases', ['pkg-1.0.0.tar.gz'])).toEqual([
      'packages/pkg/versions/pkg-1.0.0.tar.gz',
    ])
    expect(await paths('raw-releases', ['notes.txt'], { directory: 'docs/new' })).toEqual([
      'docs/new/notes.txt',
    ])
  })

  it('replays the first answer for the same idempotency key', async () => {
    const first = await upload('npm-releases', ['replay-1.0.0.tgz'], {}, 'admin', 'ui-replay-1')
    const again = await upload('npm-releases', ['replay-1.0.0.tgz'], {}, 'admin', 'ui-replay-1')
    expect(again.headers.get('Idempotent-Replayed')).toBe('true')
    expect(again.body.component.id).toBe(first.body.component.id)
  })

  it('applies the write policy: deny refuses, allow replaces and says what it replaced', async () => {
    const repo = repositories.find((r) => r.name === 'npm-releases')!
    repositories.splice(repositories.indexOf(repo), 1, { ...repo, write_policy: 'deny' })
    expect((await upload('npm-releases', ['a-1.0.0.tgz'])).body.error.details.policy).toBe('deny')
    repositories.splice(
      repositories.indexOf(repositories.find((r) => r.name === 'npm-releases')!),
      1,
      { ...repo, write_policy: 'allow' },
    )
    await upload('npm-releases', ['dup-1.0.0.tgz'])
    const replaced = await upload('npm-releases', ['dup-1.0.0.tgz'])
    expect(replaced.status).toBe(201)
    expect(replaced.body.replaced).toEqual(['dup/-/dup-1.0.0.tgz'])
  })

  it('answers the other refusals of the contract', async () => {
    expect((await upload('maven-proxy', ['x-1.0.jar'])).body.error.code).toBe(
      'repository_not_hosted',
    )
    expect((await upload('nope', ['x-1.0.jar'])).status).toBe(404)
    expect(
      nth((await upload('raw-releases', ['x.txt'], {}, 'alice')).body.error.details.fields, 0)
        .field,
    ).toBe('/fields/directory')
    expect((await upload('maven-releases', ['x-1.0.jar'], {}, null)).status).toBe(401)
    expect((await upload('npm-releases', ['ratelimited-1.0.0.tgz'])).status).toBe(429)
    expect((await upload('npm-releases', ['unavailable-1.0.0.tgz'])).status).toBe(503)
    const noFiles = await upload('npm-releases', [])
    expect(noFiles.body.error.code).toBe('invalid_input')
  })

  it('checks write access: a developer may write to a hosted repository, a reader may not', async () => {
    expect((await upload('npm-releases', ['dev-1.0.0.tgz'], {}, 'alice')).status).toBe(201)
    const proxyReader = await upload('maven-group', ['g-1.0.jar'], {}, 'alice')
    expect(proxyReader.body.error.code).toBe('repository_not_hosted')
  })
})
