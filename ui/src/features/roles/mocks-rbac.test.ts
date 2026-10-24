import { beforeEach, describe, expect, it } from 'vitest'
import { nth } from '@/test/api-json'
import { call, idOf, readEtag, useIdentityMock } from './test-support'

beforeEach(() => useIdentityMock())

async function roleId(name: string): Promise<string> {
  return idOf(name, (await call('GET', '/api/v1/roles')).json.items)
}

describe('roles mock', () => {
  it('lists, filters and pages roles', async () => {
    const all = await call('GET', '/api/v1/roles')
    expect(all.status).toBe(200)
    expect(all.json.items.map((r: { name: string }) => r.name)).toContain('nx-admin')
    expect((await call('GET', '/api/v1/roles?q=npm')).json.items).toHaveLength(1)
    const custom = await call('GET', '/api/v1/roles?builtin=false')
    expect(custom.json.items.every((r: { builtin: boolean }) => !r.builtin)).toBe(true)
    const first = await call('GET', '/api/v1/roles?limit=2')
    expect(first.json.next).not.toBeNull()
    const pageToken = first.json.next
    if (pageToken === null) throw new Error('expected a page token')
    const second = await call('GET', `/api/v1/roles?limit=2&after=${pageToken}`)
    expect(second.status).toBe(200)
    // A token is bound to its filters: another filter makes it invalid.
    const wrong = await call('GET', `/api/v1/roles?limit=2&q=a&after=${pageToken}`)
    expect(wrong.status).toBe(400)
    expect(wrong.json.error.code).toBe('invalid_page_token')
    expect((await call('GET', '/api/v1/roles?limit=0')).json.error.code).toBe('invalid_query')
  })

  it('validates input and refuses a duplicate name', async () => {
    const empty = await call('POST', '/api/v1/roles', { body: { name: ' ' } })
    expect(empty.status).toBe(400)
    expect(nth(empty.json.error.details.fields, 0).field).toBe('/name')
    const unknown = await call('POST', '/api/v1/roles', {
      body: { name: 'x', privilege_ids: ['nope'], nested_role_ids: ['nope'] },
    })
    expect(unknown.json.error.details.fields).toHaveLength(2)
    const duplicate = await call('POST', '/api/v1/roles', { body: { name: 'NX-Admin' } })
    expect(duplicate.status).toBe(409)
    expect(duplicate.json.error.code).toBe('already_exists')
  })

  it('creates, reads with an ETag and replaces only with the matching one', async () => {
    const created = await call('POST', '/api/v1/roles', { body: { name: 'qa', description: 'QA' } })
    expect(created.status).toBe(201)
    const path = `/api/v1/roles/${created.json.id}`
    const { etag } = await readEtag(path)
    expect(etag).toMatch(/^"mock-/)
    const body = { name: 'qa-team', description: '', privilege_ids: [], nested_role_ids: [] }
    expect((await call('PUT', path, { body })).status).toBe(428)
    expect((await call('PUT', path, { body, headers: { 'if-match': '"stale"' } })).status).toBe(412)
    const saved = await call('PUT', path, { body, headers: { 'if-match': etag } })
    expect(saved.status).toBe(200)
    expect(saved.json.name).toBe('qa-team')
    expect(saved.headers.get('etag')).not.toBe(etag)
    expect((await call('GET', '/api/v1/roles/missing')).json.error.code).toBe('role_not_found')
  })

  it('keeps built-in roles read only and rejects a cycle', async () => {
    const admin = await roleId('nx-admin')
    const { etag } = await readEtag(`/api/v1/roles/${admin}`)
    const put = await call('PUT', `/api/v1/roles/${admin}`, {
      body: { name: 'nx-admin' },
      headers: { 'if-match': etag },
    })
    expect(put.json.error.code).toBe('builtin_read_only')
    expect((await call('DELETE', `/api/v1/roles/${admin}`)).json.error.code).toBe(
      'builtin_read_only',
    )

    const maven = await roleId('maven-publishers')
    const managers = await roleId('release-managers')
    const read = await readEtag(`/api/v1/roles/${maven}`)
    const cycle = await call('PUT', `/api/v1/roles/${maven}`, {
      body: { name: 'maven-publishers', nested_role_ids: [managers] },
      headers: { 'if-match': read.etag },
    })
    expect(cycle.status).toBe(409)
    expect(cycle.json.error.code).toBe('role_cycle')
  })

  it('refuses to delete a role in use unless forced, and the force removes the assignments', async () => {
    const maven = await roleId('maven-publishers')
    const refused = await call('DELETE', `/api/v1/roles/${maven}`)
    expect(refused.status).toBe(409)
    expect(refused.json.error.code).toBe('resource_in_use')
    const kinds = refused.json.error.details.dependents.map((d: { kind: string }) => d.kind)
    expect(kinds).toEqual(expect.arrayContaining(['user', 'idp_group_mapping', 'role']))
    const forced = await call('DELETE', `/api/v1/roles/${maven}?force=true`)
    expect(forced.status).toBe(204)
    expect((await call('GET', `/api/v1/roles/${maven}`)).status).toBe(404)
    const bob = nth((await call('GET', '/api/v1/users?q=bob')).json.items, 0)
    expect(bob.roles.map((r: { role: { name: string } }) => r.role.name)).not.toContain(
      'maven-publishers',
    )
  })

  it('deletes an unused role with a matching optional If-Match only', async () => {
    const created = await call('POST', '/api/v1/roles', { body: { name: 'tmp' } })
    const path = `/api/v1/roles/${created.json.id}`
    expect((await call('DELETE', path, { headers: { 'if-match': '"stale"' } })).status).toBe(412)
    expect((await call('DELETE', path)).status).toBe(204)
  })

  it('computes the effective privileges through nesting', async () => {
    const managers = await roleId('release-managers')
    const reply = await call('GET', `/api/v1/roles/${managers}/effective-privileges`)
    const names = reply.json.items.map((p: { name: string }) => p.name)
    expect(names).toEqual(
      expect.arrayContaining(['raw-all', 'maven-releases-write', 'docker-push']),
    )
  })

  it('answers 403 to a user who is not an administrator', async () => {
    useIdentityMock('alice')
    expect((await call('GET', '/api/v1/roles')).json.error.code).toBe('forbidden')
  })

  it('fails on request with the chaos switch', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'roles:503')
    const down = await call('GET', '/api/v1/roles')
    expect(down.status).toBe(503)
    expect(down.headers.get('retry-after')).toBe('5')
    window.sessionStorage.setItem('ryx-mock-chaos', 'roles:429,users:403')
    expect((await call('GET', '/api/v1/roles')).status).toBe(429)
    expect((await call('GET', '/api/v1/users')).status).toBe(403)
    window.sessionStorage.setItem('ryx-mock-chaos', 'roles:200')
    expect((await call('GET', '/api/v1/roles')).status).toBe(200)
  })
})

describe('privileges mock', () => {
  it('lists with filters', async () => {
    const all = await call('GET', '/api/v1/privileges')
    expect(all.json.items.length).toBeGreaterThan(10)
    expect((await call('GET', '/api/v1/privileges?kind=admin')).json.items).toHaveLength(1)
    expect((await call('GET', '/api/v1/privileges?builtin=true')).json.items).toHaveLength(3)
    expect((await call('GET', '/api/v1/privileges?q=maven')).json.items).toHaveLength(2)
    const star = await call('GET', '/api/v1/privileges?repository=*')
    expect(
      star.json.items.every((p: { grant: { repository: string } }) => p.grant.repository === '*'),
    ).toBe(true)
    expect(
      (await call('GET', '/api/v1/privileges?repository=maven')).json.items.length,
    ).toBeGreaterThan(1)
  })

  it('validates grants, actions, repositories and selectors', async () => {
    const admin = await call('POST', '/api/v1/privileges', {
      body: { name: 'a', grant: { type: 'admin' }, actions: ['browse'] },
    })
    expect(admin.json.error.details.fields.map((f: { field: string }) => f.field)).toContain(
      '/grant/type',
    )
    const none = await call('POST', '/api/v1/privileges', {
      body: {
        name: 'a',
        grant: { type: 'repository-view', repository: '*', format: null },
        actions: [],
      },
    })
    expect(nth(none.json.error.details.fields, 0).field).toBe('/actions')
    const unknownRepo = await call('POST', '/api/v1/privileges', {
      body: {
        name: 'a',
        grant: { type: 'repository-view', repository: 'nope', format: null },
        actions: ['read'],
      },
    })
    expect(nth(unknownRepo.json.error.details.fields, 0).field).toBe('/grant/repository')
    const unknownSelector = await call('POST', '/api/v1/privileges', {
      body: {
        name: 'a',
        grant: { type: 'repository-content-selector', repository: '*', selector_id: 'nope' },
        actions: ['read'],
      },
    })
    expect(nth(unknownSelector.json.error.details.fields, 0).field).toBe('/grant/selector_id')
    const duplicate = await call('POST', '/api/v1/privileges', {
      body: {
        name: 'NPM-READ',
        grant: { type: 'repository-view', repository: '*', format: null },
        actions: ['read'],
      },
    })
    expect(duplicate.json.error.code).toBe('already_exists')
  })

  it('creates, replaces with the ETag, protects built-ins and detaches on force delete', async () => {
    const created = await call('POST', '/api/v1/privileges', {
      body: {
        name: 'raw-read',
        grant: { type: 'repository-view', repository: 'raw-releases', format: 'raw' },
        actions: ['browse', 'read'],
      },
    })
    expect(created.status).toBe(201)
    const path = `/api/v1/privileges/${created.json.id}`
    const { etag, json } = await readEtag(path)
    const body = { name: json.name, description: 'x', grant: json.grant, actions: ['read'] }
    expect((await call('PUT', path, { body, headers: { 'if-match': etag } })).status).toBe(200)
    expect((await call('PUT', path, { body, headers: { 'if-match': etag } })).status).toBe(412)

    const builtin = nth((await call('GET', '/api/v1/privileges?builtin=true')).json.items, 0)
    const bPath = `/api/v1/privileges/${builtin.id}`
    const read = await readEtag(bPath)
    const put = await call('PUT', bPath, {
      body: { name: builtin.name, grant: builtin.grant, actions: builtin.actions },
      headers: { 'if-match': read.etag },
    })
    expect(put.json.error.code).toBe('builtin_read_only')
    expect((await call('DELETE', bPath)).json.error.code).toBe('builtin_read_only')

    const npm = nth((await call('GET', '/api/v1/privileges?q=npm-read')).json.items, 0)
    const refused = await call('DELETE', `/api/v1/privileges/${npm.id}`)
    expect(refused.json.error.code).toBe('resource_in_use')
    expect(nth(refused.json.error.details.dependents, 0).kind).toBe('role')
    expect((await call('DELETE', `/api/v1/privileges/${npm.id}?force=true`)).status).toBe(204)
    expect((await call('GET', `/api/v1/privileges/${npm.id}`)).json.error.code).toBe(
      'privilege_not_found',
    )
  })
})

describe('content selectors mock', () => {
  it('validates, tests and previews an expression', async () => {
    const ok = await call('POST', '/api/v1/content-selectors/validate', {
      body: { expression: 'format == "maven2" && path.startsWith("/com/")' },
    })
    expect(ok.json).toEqual({ valid: true, errors: [] })
    const bad = await call('POST', '/api/v1/content-selectors/validate', {
      body: { expression: 'format == "maven2" &&' },
    })
    expect(bad.json.valid).toBe(false)
    expect(nth(bad.json.errors, 0).kind).toBe('syntax')
    expect(nth(bad.json.errors, 0).offset).toBeGreaterThan(0)

    const samples = [
      {
        format: 'maven2',
        path: '/com/example/a.jar',
        repository: 'r',
        coordinate: { group: 'com.example', name: 'a', version: '1' },
      },
      { format: 'npm', path: '/x', repository: 'r' },
      {
        format: 'docker',
        path: '/v2/x',
        repository: 'r',
        coordinate: { group: null, name: null, version: null },
      },
    ]
    const tested = await call('POST', '/api/v1/content-selectors/test', {
      body: { expression: 'format == "maven2" || coordinate.name == "x"', samples },
    })
    expect(tested.json.results.map((r: { result: string }) => r.result)).toEqual([
      'match',
      'unknown',
      'unknown',
    ])
    expect(nth(tested.json.results, 1).reason).toContain('coordinate.name')
    const invalid = await call('POST', '/api/v1/content-selectors/test', {
      body: { expression: '(', samples },
    })
    expect(invalid.json.valid).toBe(false)
    expect(
      (
        await call('POST', '/api/v1/content-selectors/test', {
          body: { expression: 'format == "npm"', samples: [] },
        })
      ).status,
    ).toBe(400)

    const preview = await call('POST', '/api/v1/content-selectors/preview', {
      body: { expression: 'path.startsWith("/com/example/")', repository: 'maven-releases' },
    })
    expect(preview.json.items.length).toBeGreaterThan(0)
    expect(
      (
        await call('POST', '/api/v1/content-selectors/preview', {
          body: { expression: 'x', repository: 'maven-releases' },
        })
      ).json.valid,
    ).toBe(false)
    expect(
      (
        await call('POST', '/api/v1/content-selectors/preview', {
          body: { expression: 'format == "x"', repository: 'nope' },
        })
      ).status,
    ).toBe(404)
    const language = await call('GET', '/api/v1/content-selectors/language')
    expect(language.json.limits.max_expression_bytes).toBe(1024)
  })

  it('creates, rejects a bad expression with compile errors, updates and deletes', async () => {
    const bad = await call('POST', '/api/v1/content-selectors', {
      body: { name: 'bad', expression: 'path ==' },
    })
    expect(bad.status).toBe(400)
    expect(bad.json.error.code).toBe('invalid_selector')
    expect(nth(bad.json.error.details.compile_errors, 0).message).toContain('syntax')
    expect(
      (
        await call('POST', '/api/v1/content-selectors', {
          body: { name: '', expression: 'format == "x"' },
        })
      ).status,
    ).toBe(400)
    expect(
      (
        await call('POST', '/api/v1/content-selectors', {
          body: { name: 'internal-libs', expression: 'format == "x"' },
        })
      ).json.error.code,
    ).toBe('already_exists')

    const created = await call('POST', '/api/v1/content-selectors', {
      body: { name: 'npm-only', expression: 'format == "npm"' },
    })
    expect(created.status).toBe(201)
    const path = `/api/v1/content-selectors/${created.json.id}`
    const { etag } = await readEtag(path)
    const body = { name: 'npm-only', description: 'd', expression: 'format in ["npm", "pypi"]' }
    expect((await call('PUT', path, { body, headers: { 'if-match': etag } })).status).toBe(200)
    expect((await call('PUT', path, { body, headers: { 'if-match': etag } })).status).toBe(412)
    expect((await call('DELETE', path)).status).toBe(204)

    const used = nth((await call('GET', '/api/v1/content-selectors?q=internal')).json.items, 0)
    const blocked = await call('DELETE', `/api/v1/content-selectors/${used.id}`)
    expect(blocked.json.error.code).toBe('resource_in_use')
    expect(nth(blocked.json.error.details.dependents, 0).kind).toBe('privilege')
    expect((await call('GET', '/api/v1/content-selectors/missing')).json.error.code).toBe(
      'content_selector_not_found',
    )
  })
})
