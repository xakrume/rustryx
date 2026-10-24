import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { nth } from '@/test/api-json'
import { commit, identity } from './mock-store'
import { call, protectRepositories, useIdentityMock } from './test-support'

let restore: () => void
beforeEach(() => {
  useIdentityMock()
  restore = protectRepositories()
})
afterEach(() => restore())

describe('anonymous access mock', () => {
  it('reports the switches and counts', async () => {
    const state = await call('GET', '/api/v1/anonymous-access')
    expect(state.json.global_enabled).toBe(true)
    expect(state.json.global_editable).toBe(false)
    expect(state.json.anonymous_role.name).toBe('nx-anonymous')
    expect(state.json.effective_repository_count).toBe(state.json.public_repository_count)
    expect(state.headers.get('etag')).toMatch(/^"mock-/)
  })

  it('keeps the global switch read only while it comes from the configuration', async () => {
    const { headers } = await call('GET', '/api/v1/anonymous-access')
    const etag = headers.get('etag') ?? ''
    expect(
      (await call('PUT', '/api/v1/anonymous-access', { body: { global_enabled: false } })).status,
    ).toBe(428)
    expect(
      (
        await call('PUT', '/api/v1/anonymous-access', {
          body: { global_enabled: 'no' },
          headers: { 'if-match': etag },
        })
      ).status,
    ).toBe(400)
    const refused = await call('PUT', '/api/v1/anonymous-access', {
      body: { global_enabled: false },
      headers: { 'if-match': etag },
    })
    expect(refused.status).toBe(409)
    expect(refused.json.error.code).toBe('setting_managed_by_config')
  })

  it('changes the global switch when the server makes it editable, and the effective flags follow', async () => {
    commit({ anonymous: { ...identity().anonymous, editable: true } })
    const { headers } = await call('GET', '/api/v1/anonymous-access')
    const off = await call('PUT', '/api/v1/anonymous-access', {
      body: { global_enabled: false },
      headers: { 'if-match': headers.get('etag') ?? '' },
    })
    expect(off.status).toBe(200)
    expect(off.json.effective_repository_count).toBe(0)
    expect(off.json.public_repository_count).toBeGreaterThan(0)
    const repo = (await call('GET', '/api/v1/repositories?limit=1000')).json.items.find(
      (r: { allow_anonymous: boolean }) => r.allow_anonymous,
    )
    if (repo === undefined) throw new Error('expected a repository with anonymous access')
    expect(repo.anonymous_effective).toBe(false)
  })

  it('changes repository flags in one go, all or nothing', async () => {
    const before = (await call('GET', '/api/v1/anonymous-access')).json.public_repository_count
    const ok = await call('PUT', '/api/v1/anonymous-access/repositories', {
      body: {
        repositories: [
          { name: 'maven-releases', allow_anonymous: true },
          { name: 'npm-releases', allow_anonymous: false },
        ],
      },
    })
    expect(ok.json.updated).toBeGreaterThanOrEqual(1)
    const unknown = await call('PUT', '/api/v1/anonymous-access/repositories', {
      body: {
        repositories: [
          { name: 'pypi-releases', allow_anonymous: true },
          { name: 'nope', allow_anonymous: true },
        ],
      },
    })
    expect(unknown.status).toBe(404)
    const pypi = nth((await call('GET', '/api/v1/repositories?q=pypi-releases')).json.items, 0)
    expect(pypi.allow_anonymous).toBe(false)
    expect(
      (await call('PUT', '/api/v1/anonymous-access/repositories', { body: { repositories: [] } }))
        .status,
    ).toBe(400)
    expect((await call('GET', '/api/v1/anonymous-access')).json.public_repository_count).not.toBe(
      before + 5,
    )
  })
})

describe('audit mock', () => {
  it('lists newest first with keyset pages and filters', async () => {
    const page = await call('GET', '/api/v1/audit-events?limit=10')
    expect(page.json.items).toHaveLength(10)
    const times = page.json.items.map((e: { at: string }) => e.at)
    expect([...times].sort().reverse()).toEqual(times)
    const pageToken = page.json.next
    if (pageToken === null) throw new Error('expected a page token')
    const next = await call('GET', `/api/v1/audit-events?limit=10&after=${pageToken}`)
    expect(nth(next.json.items, 0).id).not.toBe(nth(page.json.items, 0).id)
    expect(
      (await call('GET', '/api/v1/audit-events?limit=500')).json.items.length,
    ).toBeLessThanOrEqual(200)
    const oldest = await call('GET', '/api/v1/audit-events?sort=at&limit=5')
    expect(nth(oldest.json.items, 0).at <= nth(oldest.json.items, 4).at).toBe(true)
    expect((await call('GET', '/api/v1/audit-events?sort=kind')).json.error.code).toBe(
      'unsupported_sort',
    )

    const failures = await call('GET', '/api/v1/audit-events?outcome=failure&limit=200')
    expect(failures.json.items.length).toBeGreaterThan(0)
    expect(failures.json.items.every((e: { outcome: string }) => e.outcome === 'failure')).toBe(
      true,
    )
    const rbac = await call('GET', '/api/v1/audit-events?kind=rbac_change,token_change&limit=200')
    expect(
      rbac.json.items.every((e: { kind: string }) =>
        ['rbac_change', 'token_change'].includes(e.kind),
      ),
    ).toBe(true)
    expect(
      (await call('GET', '/api/v1/audit-events?action=role.&limit=200')).json.items.every(
        (e: { action: string }) => e.action.startsWith('role.'),
      ),
    ).toBe(true)
    expect(
      (await call('GET', '/api/v1/audit-events?action=token.revoke&limit=200')).json.items.length,
    ).toBeGreaterThan(0)
    expect(
      (await call('GET', '/api/v1/audit-events?actor=BOB&limit=200')).json.items.length,
    ).toBeGreaterThan(0)
    const target = rbac.json.items.find((e: { target: string | null }) => e.target !== null)
    if (target === undefined || target.target === null) {
      throw new Error('expected an event with a target')
    }
    expect(
      (
        await call(
          'GET',
          `/api/v1/audit-events?target=${encodeURIComponent(target.target)}&limit=200`,
        )
      ).json.items.length,
    ).toBeGreaterThan(0)
    expect(
      (await call('GET', '/api/v1/audit-events?repository=maven-releases&limit=200')).json.items
        .length,
    ).toBeGreaterThan(0)
    expect(
      (await call('GET', '/api/v1/audit-events?project=payments&limit=200')).json.items.length,
    ).toBeGreaterThan(0)
    expect(
      (await call('GET', `/api/v1/audit-events?request_id=${nth(page.json.items, 0).request_id}`))
        .json.items,
    ).toHaveLength(1)
    const from = new Date(Date.now() - 86_400_000).toISOString()
    const recent = await call('GET', `/api/v1/audit-events?from=${from}&limit=200`)
    expect(
      recent.json.items.every((e: { at: string }) => e.at >= from.replace(/\.\d{3}Z$/, 'Z')),
    ).toBe(true)
    expect(
      (await call('GET', `/api/v1/audit-events?to=${from}&limit=200`)).json.items.length,
    ).toBeGreaterThan(0)
  })

  it('records the changes the other mocks make', async () => {
    await call('POST', '/api/v1/roles', { body: { name: 'audited-role' } })
    const events = await call('GET', '/api/v1/audit-events?limit=1')
    const event = nth(events.json.items, 0)
    expect(event.action).toBe('role.create')
    expect(event.target).toBe('audited-role')
    expect(event.actor_name).toBe('admin')
    const one = await call('GET', `/api/v1/audit-events/${event.id}`)
    expect(one.json.kind).toBe('rbac_change')
    expect((await call('GET', '/api/v1/audit-events/missing')).json.error.code).toBe(
      'audit_event_not_found',
    )
  })

  it('streams the export oldest first, one JSON line with a cursor each, and resumes after a cursor', async () => {
    const response = await fetch(
      'http://localhost:3000/api/v1/audit-events/export?outcome=failure',
      {
        headers: { 'x-csrf-token': 'x' },
      },
    )
    expect(response.headers.get('content-type')).toBe('application/x-ndjson')
    expect(response.headers.get('content-disposition')).toContain('attachment')
    const text = await response.text()
    expect(text.endsWith('\n')).toBe(true)
    const lines = text
      .trimEnd()
      .split('\n')
      .map((l) => JSON.parse(l) as { at: string; cursor: string })
    expect(lines.length).toBeGreaterThan(1)
    expect(lines.map((l) => l.at)).toEqual([...lines.map((l) => l.at)].sort())
    const resumed = await fetch(
      `http://localhost:3000/api/v1/audit-events/export?outcome=failure&after=${lines[0]?.cursor}`,
    )
    expect((await resumed.text()).trimEnd().split('\n')).toHaveLength(lines.length - 1)
  })

  it('refuses a range longer than 366 days and answers 403 to non administrators', async () => {
    const wide = await call(
      'GET',
      '/api/v1/audit-events/export?from=2024-01-01T00:00:00Z&to=2026-01-01T00:00:00Z',
    )
    expect(wide.status).toBe(400)
    expect(wide.json.error.code).toBe('range_too_large')
    expect(wide.json.error.details.max_days).toBe(366)
    useIdentityMock('alice')
    expect((await call('GET', '/api/v1/audit-events')).status).toBe(403)
  })

  it('cuts the export halfway once when asked to, then streams whole', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'audit-export:cut')
    const cut = await fetch('http://localhost:3000/api/v1/audit-events/export')
    await expect(cut.text()).rejects.toThrow()
    expect(window.sessionStorage.getItem('ryx-mock-chaos')).toBe('')
    const whole = await fetch('http://localhost:3000/api/v1/audit-events/export')
    expect((await whole.text()).endsWith('\n')).toBe(true)
  })
})
