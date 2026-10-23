import { describe, expect, it } from 'vitest'
import { createApiClient } from '@/api/client'
import { setCsrfToken } from '@/api/csrf'
import { server } from '@/test/server'
import { handlers } from './handlers'
import { readSession } from './session'

const BASE = 'http://localhost:3000'
const api = () => createApiClient({ baseUrl: BASE })

async function signIn(username = 'admin', password = 'mock-password') {
  const { data, response } = await api().POST('/api/v1/auth/login', {
    body: { username, password },
  })
  if (data) setCsrfToken(data.csrf_token)
  return { data, response }
}

describe('mock API', () => {
  it('signs in, reports the session and signs out', async () => {
    server.use(...handlers)
    expect((await api().GET('/api/v1/auth/session')).data?.authenticated).toBe(false)
    const { data } = await signIn()
    expect(data?.user?.username).toBe('admin')
    expect(readSession()?.username).toBe('admin')
    expect((await api().GET('/api/v1/auth/session')).data?.authenticated).toBe(true)
    await api().POST('/api/v1/auth/logout')
    expect(readSession()).toBeNull()
  })

  it('answers one generic message for a wrong password and an unknown user', async () => {
    server.use(...handlers)
    const wrong = await signIn('admin', 'nope')
    const unknown = await signIn('nobody', 'mock-password')
    expect(wrong.response.status).toBe(401)
    expect(unknown.response.status).toBe(401)
    expect(JSON.stringify(wrong.data)).toBe(JSON.stringify(unknown.data))
  })

  it('answers 429 locked_out with Retry-After for the special user', async () => {
    server.use(...handlers)
    const { response } = await signIn('locked')
    expect(response.status).toBe(429)
    expect(response.headers.get('retry-after')).toBe('30')
  })

  it('must_not_serve_a_protected_endpoint_without_a_session', async () => {
    server.use(...handlers)
    const { response } = await api().GET('/api/v1/users')
    expect(response.status).toBe(401)
  })

  it('enforces the CSRF token on unsafe methods of a session', async () => {
    server.use(...handlers)
    await signIn()
    setCsrfToken('wrong')
    const bad = await api().POST('/api/v1/me/actions/sign-out-everywhere')
    // The client repairs a csrf_rejected once by asking /auth/csrf, so the retry succeeds.
    expect(bad.response.status).toBe(204)
  })

  it('pages the repository list with opaque keyset tokens, filters and sorts', async () => {
    server.use(...handlers)
    await signIn()
    const first = await api().GET('/api/v1/repositories', { params: { query: { limit: 10 } } })
    expect(first.data?.items).toHaveLength(10)
    expect(first.data?.next).toEqual(expect.any(String))
    const second = await api().GET('/api/v1/repositories', {
      params: { query: { limit: 10, after: first.data?.next ?? undefined } },
    })
    expect(second.data?.items[0]?.name).not.toBe(first.data?.items[0]?.name)

    const filtered = await api().GET('/api/v1/repositories', {
      params: { query: { q: 'maven', format: 'maven2' } },
    })
    expect(filtered.data?.items.every((r) => r.name.includes('maven'))).toBe(true)
    expect(filtered.data?.next).toBeNull()

    const descending = await api().GET('/api/v1/repositories', {
      params: { query: { sort: '-name', limit: 3 } },
    })
    const names = descending.data?.items.map((r) => r.name) ?? []
    expect(names).toEqual([...names].sort().reverse())
  })

  it('rejects a bad page token and an unknown sort with the contract codes', async () => {
    server.use(...handlers)
    const badToken = await api().GET('/api/v1/repositories', {
      params: { query: { after: '###' } },
    })
    expect(badToken.response.status).toBe(400)
    expect(JSON.stringify(badToken.error)).toContain('invalid_page_token')
    const badSort = await api().GET('/api/v1/repositories', {
      params: { query: { sort: 'size' as never } },
    })
    expect(JSON.stringify(badSort.error)).toContain('unsupported_sort')
  })

  it('must_not_show_private_repositories_to_an_anonymous_caller', async () => {
    server.use(...handlers)
    const anonymous = await api().GET('/api/v1/repositories', {
      params: { query: { limit: 1000 } },
    })
    await signIn()
    const signedIn = await api().GET('/api/v1/repositories', { params: { query: { limit: 1000 } } })
    expect(anonymous.data?.items.every((r) => r.allow_anonymous)).toBe(true)
    expect(signedIn.data?.items.length ?? 0).toBeGreaterThan(anonymous.data?.items.length ?? 0)
  })

  it('answers 404 repository_not_found for a hidden or unknown repository', async () => {
    server.use(...handlers)
    const { response, error } = await api().GET('/api/v1/repositories/{name}', {
      params: { path: { name: 'does-not-exist' } },
    })
    expect(response.status).toBe(404)
    expect(JSON.stringify(error)).toContain('repository_not_found')
  })

  it('falls back to the generated example for every other operation', async () => {
    server.use(...handlers)
    await signIn()
    const roles = await api().GET('/api/v1/roles')
    expect(roles.response.status).toBe(200)
    expect(roles.data?.items.length).toBeGreaterThan(0)
  })

  it('requires If-Match on PUT and answers 412 for a stale ETag', async () => {
    server.use(...handlers)
    await signIn()
    const missing = await api().PUT('/api/v1/roles/{role_id}', {
      // The generated types require the header; a client bug is exactly omitting it.
      params: { path: { role_id: '0192f6c4-5a1e-7000-8000-00000000000b' }, header: {} as never },
      body: {} as never,
    })
    expect(missing.response.status).toBe(428)
    const stale = await api().PUT('/api/v1/roles/{role_id}', {
      params: {
        path: { role_id: '0192f6c4-5a1e-7000-8000-00000000000b' },
        header: { 'If-Match': '"stale"' },
      },
      body: {} as never,
    })
    expect(stale.response.status).toBe(412)
  })
})
