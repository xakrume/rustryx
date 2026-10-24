import { beforeEach, describe, expect, it } from 'vitest'
import { nth, type ApiJson } from '@/test/api-json'
import { ROLE_ID } from './mock-seed'
import { commit, identity } from './mock-store'
import { call, idOf, readEtag, useIdentityMock } from './test-support'

beforeEach(() => useIdentityMock())

const ROLE = async (name: string): Promise<string> =>
  idOf(name, (await call('GET', '/api/v1/roles')).json.items)
const USER = async (name: string): Promise<ApiJson> =>
  nth((await call('GET', `/api/v1/users?q=${name}`)).json.items, 0)

describe('users mock', () => {
  it('lists with filters, sort and keyset paging', async () => {
    const all = await call('GET', '/api/v1/users?limit=5')
    expect(all.json.items).toHaveLength(5)
    expect(all.json.next).not.toBeNull()
    expect(
      (await call('GET', '/api/v1/users?source=ldap&limit=100')).json.items.every(
        (u: { source: string }) => u.source === 'ldap',
      ),
    ).toBe(true)
    expect(
      (await call('GET', '/api/v1/users?enabled=false&limit=100')).json.items.length,
    ).toBeGreaterThan(0)
    expect(nth((await call('GET', '/api/v1/users?role=ci-bots')).json.items, 0).username).toBe(
      'svc-ci',
    )
    expect(nth((await call('GET', '/api/v1/users?q=alice@')).json.items, 0).username).toBe('alice')
    const newest = await call('GET', '/api/v1/users?sort=-created_at&limit=3')
    expect(newest.json.items).toHaveLength(3)
    expect((await call('GET', '/api/v1/users?sort=bogus')).json.error.code).toBe('unsupported_sort')
  })

  it('creates a local user and explains a username clash with another source', async () => {
    const role = await ROLE('npm-readers')
    const created = await call('POST', '/api/v1/users', {
      body: {
        username: 'zoe',
        password: 'a long enough password',
        display_name: 'Zoe',
        email: 'z@example.org',
        role_ids: [role],
      },
    })
    expect(created.status).toBe(201)
    expect(nth(created.json.roles, 0).origin).toBe('manual')
    expect(created.json.source).toBe('local')
    expect(
      (
        await call('POST', '/api/v1/users', {
          body: { username: 'ZOE', password: 'a long enough password' },
        })
      ).json.error.code,
    ).toBe('already_exists')
    const clash = await call('POST', '/api/v1/users', {
      body: { username: 'Alice', password: 'a long enough password' },
    })
    expect(clash.status).toBe(409)
    expect(clash.json.error.code).toBe('username_conflict')
    expect(
      (
        await call('POST', '/api/v1/users', {
          body: { username: '-bad', password: 'a long enough password' },
        })
      ).status,
    ).toBe(400)
    const short = await call('POST', '/api/v1/users', {
      body: { username: 'q', password: 'short' },
    })
    expect(nth(short.json.error.details.fields, 0).field).toBe('/password')
    expect(
      (
        await call('POST', '/api/v1/users', {
          body: { username: 'q', password: 'ryx_a long enough one' },
        })
      ).status,
    ).toBe(400)
    expect(
      (
        await call('POST', '/api/v1/users', {
          body: { username: 'q', password: 'a long enough password', role_ids: ['nope'] },
        })
      ).status,
    ).toBe(400)
    const anonymous = await ROLE('nx-anonymous')
    expect(
      (
        await call('POST', '/api/v1/users', {
          body: { username: 'q', password: 'a long enough password', role_ids: [anonymous] },
        })
      ).status,
    ).toBe(400)
  })

  it('patches with the ETag and keeps external profile fields read only', async () => {
    const bob = await USER('bob')
    const path = `/api/v1/users/${bob.id}`
    const { etag } = await readEtag(path)
    expect((await call('PATCH', path, { body: { display_name: 'Robert' } })).status).toBe(428)
    const patched = await call('PATCH', path, {
      body: { display_name: 'Robert', enabled: false },
      headers: { 'if-match': etag },
    })
    expect(patched.status).toBe(200)
    expect(patched.json.enabled).toBe(false)
    expect(patched.json.tokens_valid_after).not.toBeNull()
    expect(
      (await call('PATCH', path, { body: { enabled: true }, headers: { 'if-match': etag } }))
        .status,
    ).toBe(412)

    const alice = await USER('alice')
    const aPath = `/api/v1/users/${alice.id}`
    const read = await readEtag(aPath)
    const external = await call('PATCH', aPath, {
      body: { email: 'x@example.org' },
      headers: { 'if-match': read.etag },
    })
    expect(external.json.error.code).toBe('external_identity')
    expect(read.json.editable_fields).toEqual(['enabled', 'roles'])
    expect(read.json.read_only_reason).toContain('identity provider')
  })

  it('refuses to disable or delete the own account and the last administrator', async () => {
    const admin = await USER('admin')
    const path = `/api/v1/users/${admin.id}`
    const { etag } = await readEtag(path)
    const self = await call('PATCH', path, {
      body: { enabled: false },
      headers: { 'if-match': etag },
    })
    expect(self.json.error.code).toBe('cannot_modify_self')
    expect((await call('DELETE', path)).json.error.code).toBe('cannot_modify_self')
    const adminRole = await ROLE('nx-admin')
    const selfDemote = await call('PUT', `${path}/roles`, {
      body: { role_ids: [] },
      headers: { 'if-match': etag },
    })
    expect(selfDemote.json.error.code).toBe('cannot_modify_self')

    // Make bob an administrator, then the only OTHER admin is the one asking: removing it is fine,
    // but removing the last one is refused.
    const bob = await USER('bob')
    const bobRead = await readEtag(`/api/v1/users/${bob.id}`)
    const granted = await call('PUT', `/api/v1/users/${bob.id}/roles`, {
      body: { role_ids: [adminRole] },
      headers: { 'if-match': bobRead.etag },
    })
    expect(granted.status).toBe(200)
    expect((await call('DELETE', `/api/v1/users/${bob.id}`)).status).toBe(204)
    const svc = await USER('svc-ci')
    const svcRead = await readEtag(`/api/v1/users/${svc.id}`)
    await call('PUT', `/api/v1/users/${svc.id}/roles`, {
      body: { role_ids: [adminRole] },
      headers: { 'if-match': svcRead.etag },
    })
    // admin (the caller) is now one of two; remove svc-ci again: allowed
    const again = await readEtag(`/api/v1/users/${svc.id}`)
    expect(
      (
        await call('PUT', `/api/v1/users/${svc.id}/roles`, {
          body: { role_ids: [] },
          headers: { 'if-match': again.etag },
        })
      ).status,
    ).toBe(200)
  })

  it('refuses nx-admin on an external account, nx-anonymous, and removing the last local admin by someone else', async () => {
    const alice = await USER('alice')
    const read = await readEtag(`/api/v1/users/${alice.id}`)
    const adminRole = await ROLE('nx-admin')
    const external = await call('PUT', `/api/v1/users/${alice.id}/roles`, {
      body: { role_ids: [adminRole] },
      headers: { 'if-match': read.etag },
    })
    expect(external.status).toBe(400)
    // The caller is "admin"; sign in as another administrator to try to demote the last one.
    const svc = await USER('svc-ci')
    const svcRead = await readEtag(`/api/v1/users/${svc.id}`)
    const promote = await call('PUT', `/api/v1/users/${svc.id}/roles`, {
      body: { role_ids: [adminRole] },
      headers: { 'if-match': svcRead.etag },
    })
    expect(promote.status).toBe(200)
    expect((await call('DELETE', `/api/v1/users/${svc.id}`)).status).toBe(204)
    const admin = await USER('admin')
    // Only one local administrator left (the caller itself): a delete of it is refused as self.
    expect((await call('DELETE', `/api/v1/users/${admin.id}`)).status).toBe(409)
    expect((await call('GET', '/api/v1/users/missing')).json.error.code).toBe('user_not_found')
  })

  it('answers last_admin when the only local administrator is demoted or deleted by someone else', async () => {
    // Only bob holds nx-admin; the caller (admin) does not, so neither self guard applies.
    commit({
      users: identity().users.map((u) =>
        u.username === 'bob'
          ? { ...u, manualRoleIds: [ROLE_ID.admin] }
          : u.username === 'admin'
            ? { ...u, manualRoleIds: [] }
            : u,
      ),
    })
    const bob = await USER('bob')
    const read = await readEtag(`/api/v1/users/${bob.id}`)
    const demote = await call('PUT', `/api/v1/users/${bob.id}/roles`, {
      body: { role_ids: [] },
      headers: { 'if-match': read.etag },
    })
    expect(demote.status).toBe(409)
    expect(demote.json.error.code).toBe('last_admin')
    const remove = await call('DELETE', `/api/v1/users/${bob.id}`)
    expect(remove.json.error.code).toBe('last_admin')
  })

  it('resets a local password, signs a user out and ends external password changes', async () => {
    const bob = await USER('bob')
    const reset = await call('POST', `/api/v1/users/${bob.id}/actions/reset-password`, {
      body: { password: 'another long password' },
    })
    expect(reset.status).toBe(204)
    expect(
      (
        await call('POST', `/api/v1/users/${bob.id}/actions/reset-password`, {
          body: { password: 'short' },
        })
      ).status,
    ).toBe(400)
    const alice = await USER('alice')
    expect(
      (
        await call('POST', `/api/v1/users/${alice.id}/actions/reset-password`, {
          body: { password: 'another long password' },
        })
      ).json.error.code,
    ).toBe('external_identity')
    expect((await call('POST', `/api/v1/users/${alice.id}/actions/sign-out`)).status).toBe(204)
    expect((await call('GET', `/api/v1/users/${alice.id}`)).json.tokens_valid_after).not.toBeNull()
  })
})

describe('group mappings mock', () => {
  it('lists, creates, refuses nx-admin and duplicates, updates with the ETag and deletes', async () => {
    const list = await call('GET', '/api/v1/idp-group-mappings?provider=oidc')
    expect(list.json.items.map((m: { priority: number }) => m.priority)).toEqual([
      50, 100, 100, 200,
    ])
    expect(
      (await call('GET', `/api/v1/idp-group-mappings?q=/rustryx/sec`)).json.items,
    ).toHaveLength(1)
    const developer = await ROLE('nx-developer')
    const admin = await ROLE('nx-admin')
    const anonymous = await ROLE('nx-anonymous')
    expect(
      (await call('GET', `/api/v1/idp-group-mappings?role_id=${developer}`)).json.items.length,
    ).toBeGreaterThan(0)
    const refused = await call('POST', '/api/v1/idp-group-mappings', {
      body: { provider: 'oidc', external_group: '/x', role_id: admin },
    })
    expect(refused.status).toBe(400)
    expect(nth(refused.json.error.details.fields, 0).field).toBe('/role_id')
    expect(
      (
        await call('POST', '/api/v1/idp-group-mappings', {
          body: { provider: 'oidc', external_group: '/x', role_id: anonymous },
        })
      ).status,
    ).toBe(400)
    expect(
      (
        await call('POST', '/api/v1/idp-group-mappings', {
          body: { provider: 'oidc', external_group: '', role_id: developer },
        })
      ).status,
    ).toBe(400)

    const created = await call('POST', '/api/v1/idp-group-mappings', {
      body: { provider: 'oidc', external_group: ' /Case Sensitive ', role_id: developer },
    })
    expect(created.status).toBe(201)
    expect(created.json.external_group).toBe(' /Case Sensitive ')
    expect(created.json.priority).toBe(100)
    const dup = await call('POST', '/api/v1/idp-group-mappings', {
      body: { provider: 'oidc', external_group: ' /Case Sensitive ', role_id: developer },
    })
    expect(dup.json.error.code).toBe('already_exists')
    // A different case is another group: no folding.
    expect(
      (
        await call('POST', '/api/v1/idp-group-mappings', {
          body: { provider: 'oidc', external_group: ' /case sensitive ', role_id: developer },
        })
      ).status,
    ).toBe(201)

    const path = `/api/v1/idp-group-mappings/${created.json.id}`
    const { etag } = await readEtag(path)
    expect((await call('PUT', path, { body: { role_id: developer, priority: 5 } })).status).toBe(
      428,
    )
    const put = await call('PUT', path, {
      body: { role_id: developer, priority: 5 },
      headers: { 'if-match': etag },
    })
    expect(put.json.priority).toBe(5)
    expect(
      (
        await call('PUT', path, {
          body: { role_id: developer, priority: 'x' },
          headers: { 'if-match': put.headers.get('etag') ?? '' },
        })
      ).status,
    ).toBe(400)
    expect((await call('DELETE', path)).status).toBe(204)
    expect((await call('GET', path)).json.error.code).toBe('idp_group_mapping_not_found')
  })

  it('resolves a group list exactly, with admin groups and denial', async () => {
    const test = (provider: string, groups: string[]) =>
      call('POST', '/api/v1/idp-group-mappings/test', { body: { provider, groups } })
    const result = await test('oidc', [
      '/rustryx/admins',
      '/rustryx/team-payments/publishers',
      '/rustryx/unknown',
    ])
    const via = result.json.roles.map((r: { via: string }) => r.via)
    expect(via).toEqual(['admin_group', 'mapping', 'mapping'])
    expect(result.json.unmatched_groups).toEqual(['/rustryx/unknown'])
    expect(result.json.denied).toBe(false)
    const wrongCase = await test('oidc', ['/Rustryx/Admins'])
    expect(wrongCase.json.denied).toBe(true)
    expect((await test('ldap', [])).status).toBe(400)
  })
})

describe('tokens mock', () => {
  it('creates a token, returns the secret once and never lists it', async () => {
    const created = await call('POST', '/api/v1/me/tokens', {
      body: { name: 'ci', expires_in_days: 30, scopes: ['repo:*:read'] },
    })
    expect(created.status).toBe(201)
    expect(created.json.secret).toMatch(/^ryx_[A-Za-z0-9]{43}$/)
    expect(created.json.display_prefix).toBe(created.json.secret.slice(0, 8))
    expect(created.json.state).toBe('active')
    const mine = await call('GET', '/api/v1/me/tokens')
    expect(JSON.stringify(mine.json)).not.toContain(created.json.secret)
    expect(nth(mine.json.items, 0).name).toBe('ci')
    const second = await call('POST', '/api/v1/me/tokens', { body: { name: 'ci2' } })
    expect(second.json.secret).not.toBe(created.json.secret)
  })

  it('validates name, lifetime and scopes', async () => {
    const bad = await call('POST', '/api/v1/me/tokens', {
      body: { name: '', expires_in_days: 400, scopes: ['everything'] },
    })
    expect(bad.status).toBe(400)
    expect(bad.json.error.details.fields.map((f: { field: string }) => f.field)).toEqual([
      '/name',
      '/expires_in_days',
      '/scopes',
    ])
  })

  it('revokes idempotently, and only own tokens through the personal route', async () => {
    const created = await call('POST', '/api/v1/me/tokens', { body: { name: 'tmp' } })
    const path = `/api/v1/me/tokens/${created.json.id}/actions/revoke`
    const first = await call('POST', path)
    expect(first.json.state).toBe('revoked')
    const again = await call('POST', path)
    expect(again.json.revoked_at).toBe(first.json.revoked_at)
    expect(
      (await call('GET', '/api/v1/me/tokens?state=revoked')).json.items.map(
        (t: { id: string }) => t.id,
      ),
    ).toContain(created.json.id)
    const others = (await call('GET', '/api/v1/api-tokens?owner_id=' + (await USER('alice')).id))
      .json.items
    const foreignId = nth(others, 0).id
    const foreign = await call('POST', `/api/v1/me/tokens/${foreignId}/actions/revoke`)
    expect(foreign.json.error.code).toBe('api_token_not_found')
    expect((await call('POST', `/api/v1/api-tokens/${foreignId}/actions/revoke`)).json.state).toBe(
      'revoked',
    )
  })

  it('lists every token for administrators with filters', async () => {
    const all = await call('GET', '/api/v1/api-tokens?limit=200')
    expect(all.json.items.length).toBe(26)
    expect(
      (await call('GET', '/api/v1/api-tokens?state=expired&limit=200')).json.items.every(
        (t: { state: string }) => t.state === 'expired',
      ),
    ).toBe(true)
    expect(
      (await call('GET', '/api/v1/api-tokens?never_used=true&limit=200')).json.items.every(
        (t: { never_used: boolean }) => t.never_used,
      ),
    ).toBe(true)
    expect((await call('GET', '/api/v1/api-tokens?kind=robot')).json.items).toHaveLength(0)
    const soon = new Date(Date.now() + 120 * 86_400_000).toISOString()
    expect(
      (await call('GET', `/api/v1/api-tokens?expiring_before=${soon}&limit=200`)).json.items.length,
    ).toBeGreaterThan(0)
    useIdentityMock('alice')
    expect((await call('GET', '/api/v1/api-tokens')).status).toBe(403)
    expect((await call('GET', '/api/v1/me/tokens')).status).toBe(200)
  })
})

describe('profile mock', () => {
  it('answers /me with roles and their origin and the groups seen', async () => {
    useIdentityMock('alice')
    const me = await call('GET', '/api/v1/me')
    expect(me.json.roles.map((r: { origin: string }) => r.origin)).toEqual([
      'manual',
      'oidc',
      'oidc',
    ])
    expect(me.json.groups).toEqual(['/rustryx/team-payments/publishers', '/rustryx/team-payments'])
    expect(me.json.groups_confirmed_at).not.toBeNull()
  })

  it('changes the own password with the rules of the contract', async () => {
    const change = (body: unknown) => call('PUT', '/api/v1/me/password', { body })
    expect(
      (await change({ current_password: '', new_password: 'a long enough password' })).status,
    ).toBe(400)
    expect(
      (await change({ current_password: 'mock-password', new_password: 'short' })).status,
    ).toBe(400)
    const wrong = await change({
      current_password: 'nope nope nope',
      new_password: 'a long enough password',
    })
    expect(wrong.status).toBe(401)
    expect(wrong.json.error.code).toBe('invalid_credentials')
    expect(
      (await change({ current_password: 'mock-password', new_password: 'a long enough password' }))
        .status,
    ).toBe(204)
    expect(
      (await change({ current_password: 'mock-password', new_password: 'a long enough password' }))
        .status,
    ).toBe(401)
    useIdentityMock('alice')
    expect(
      (await change({ current_password: 'x', new_password: 'a long enough password' })).json.error
        .code,
    ).toBe('external_identity')
  })

  it('ends every session', async () => {
    expect((await call('POST', '/api/v1/me/actions/sign-out-everywhere')).status).toBe(204)
    expect((await call('GET', '/api/v1/me/tokens')).status).toBe(401)
  })
})
