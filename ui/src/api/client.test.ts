import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { server } from '@/test/server'
import { createApiClient } from './client'
import { setCsrfToken } from './csrf'
import { apiEvents, type ApiEvent } from './events'
import { ApiError } from './errors'
import { unwrap, unwrapVersioned } from './request'

const BASE = 'http://localhost:3000'

function collect(): ApiEvent[] {
  const events: ApiEvent[] = []
  apiEvents.subscribe((e) => events.push(e))
  return events
}

describe('CSRF header', () => {
  it('sends X-CSRF-Token on unsafe methods of a cookie session', async () => {
    setCsrfToken('token-1')
    let seen: string | null = null
    server.use(
      http.post(`${BASE}/api/v1/auth/logout`, ({ request }) => {
        seen = request.headers.get('x-csrf-token')
        return HttpResponse.json({ redirect_url: null })
      }),
    )
    await createApiClient({ baseUrl: BASE }).POST('/api/v1/auth/logout')
    expect(seen).toBe('token-1')
  })

  it('must_not_send_the_csrf_token_on_safe_methods', async () => {
    setCsrfToken('token-1')
    let seen: string | null = 'unset'
    server.use(
      http.get(`${BASE}/api/v1/me`, ({ request }) => {
        seen = request.headers.get('x-csrf-token')
        return HttpResponse.json({})
      }),
    )
    await createApiClient({ baseUrl: BASE }).GET('/api/v1/me')
    expect(seen).toBeNull()
  })

  it('must_not_send_the_csrf_token_with_a_bearer_credential', async () => {
    setCsrfToken('token-1')
    let seen: string | null = 'unset'
    server.use(
      http.post(`${BASE}/api/v1/auth/logout`, ({ request }) => {
        seen = request.headers.get('x-csrf-token')
        return HttpResponse.json({ redirect_url: null })
      }),
    )
    await createApiClient({ baseUrl: BASE }).POST('/api/v1/auth/logout', {
      headers: { Authorization: 'Bearer ryx_example' },
    })
    expect(seen).toBeNull()
  })

  it('fetches a fresh token and repeats the request once after csrf_rejected', async () => {
    setCsrfToken('stale')
    const log: string[] = []
    server.use(
      http.get(`${BASE}/api/v1/auth/csrf`, () => {
        log.push('csrf')
        return HttpResponse.json({ csrf_token: 'fresh' })
      }),
      http.post(`${BASE}/api/v1/auth/logout`, ({ request }) => {
        const token = request.headers.get('x-csrf-token')
        log.push(`post:${token}`)
        return token === 'fresh'
          ? HttpResponse.json({ redirect_url: null })
          : HttpResponse.json(
              { error: { code: 'csrf_rejected', message: 'no', request_id: 'r' } },
              { status: 403 },
            )
      }),
    )
    const result = await createApiClient({ baseUrl: BASE }).POST('/api/v1/auth/logout')
    expect(result.response.status).toBe(200)
    expect(log).toEqual(['post:stale', 'csrf', 'post:fresh'])
  })

  it('must_not_retry_more_than_once_after_csrf_rejected', async () => {
    setCsrfToken('stale')
    let csrfCalls = 0
    let posts = 0
    server.use(
      http.get(`${BASE}/api/v1/auth/csrf`, () => {
        csrfCalls += 1
        return HttpResponse.json({ csrf_token: 'still-wrong' })
      }),
      http.post(`${BASE}/api/v1/auth/logout`, () => {
        posts += 1
        return HttpResponse.json(
          { error: { code: 'csrf_rejected', message: 'no', request_id: 'r' } },
          { status: 403 },
        )
      }),
    )
    const result = await createApiClient({ baseUrl: BASE }).POST('/api/v1/auth/logout')
    expect(result.response.status).toBe(403)
    expect(csrfCalls).toBe(1)
    expect(posts).toBe(2)
  })

  it('must_not_replay_a_multipart_upload_after_csrf_rejected', async () => {
    // Replaying would clone the stream and buffer the whole upload in memory.
    setCsrfToken('stale')
    let posts = 0
    let csrfCalls = 0
    server.use(
      http.get(`${BASE}/api/v1/auth/csrf`, () => {
        csrfCalls += 1
        return HttpResponse.json({ csrf_token: 'fresh' })
      }),
      http.post(`${BASE}/api/v1/components`, () => {
        posts += 1
        return HttpResponse.json(
          { error: { code: 'csrf_rejected', message: 'no', request_id: 'r' } },
          { status: 403 },
        )
      }),
    )
    const form = new FormData()
    form.set('file', new Blob(['data']), 'a.txt')
    const result = await createApiClient({ baseUrl: BASE }).POST('/api/v1/components', {
      params: { query: { repository: 'raw' } },
      body: {} as never,
      bodySerializer: () => form,
    } as never)
    expect(result.response.status).toBe(403)
    expect(posts).toBe(1)
    expect(csrfCalls).toBe(0)
  })
})

describe('events and errors', () => {
  it('turns a transport failure into an ApiError with status 0 and tells the banner', async () => {
    const events = collect()
    server.use(http.get(`${BASE}/api/v1/me`, () => HttpResponse.error()))
    const error = await createApiClient({ baseUrl: BASE })
      .GET('/api/v1/me')
      .catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect((error as ApiError).isNetwork).toBe(true)
    expect(events).toContainEqual({ type: 'network-failure' })
  })

  it('publishes unauthenticated on a 401, with the path', async () => {
    const events = collect()
    server.use(
      http.get(`${BASE}/api/v1/me/tokens`, () =>
        HttpResponse.json(
          { error: { code: 'token_expired', message: 'x', request_id: 'r' } },
          { status: 401 },
        ),
      ),
    )
    await createApiClient({ baseUrl: BASE }).GET('/api/v1/me/tokens')
    expect(events).toContainEqual({ type: 'unauthenticated', path: '/api/v1/me/tokens' })
  })

  it('must_not_redirect_to_login_when_the_login_itself_answers_401', async () => {
    const events = collect()
    server.use(
      http.post(`${BASE}/api/v1/auth/login`, () =>
        HttpResponse.json(
          { error: { code: 'invalid_credentials', message: 'x', request_id: 'r' } },
          { status: 401 },
        ),
      ),
    )
    await createApiClient({ baseUrl: BASE }).POST('/api/v1/auth/login', {
      body: { username: 'a', password: 'b' },
    })
    expect(events.some((e) => e.type === 'unauthenticated')).toBe(false)
  })

  it('publishes Retry-After from 429 and 503', async () => {
    const events = collect()
    server.use(
      http.get(`${BASE}/api/v1/me`, () =>
        HttpResponse.json(
          { error: { code: 'overloaded', message: 'x', request_id: 'r' } },
          { status: 503, headers: { 'Retry-After': '9' } },
        ),
      ),
    )
    await createApiClient({ baseUrl: BASE }).GET('/api/v1/me')
    expect(events).toContainEqual({
      type: 'retry-after',
      status: 503,
      seconds: 9,
      path: '/api/v1/me',
    })
  })

  it('must_not_treat_a_cancelled_request_as_a_failure', async () => {
    const events = collect()
    server.use(http.get(`${BASE}/api/v1/me`, () => HttpResponse.json({})))
    const controller = new AbortController()
    controller.abort()
    const error = await createApiClient({ baseUrl: BASE })
      .GET('/api/v1/me', { signal: controller.signal })
      .catch((e: unknown) => e)
    expect(error).not.toBeInstanceOf(ApiError)
    expect(events.some((e) => e.type === 'network-failure')).toBe(false)
  })

  it('serializes list parameters comma separated, as the contract says', async () => {
    let url = ''
    server.use(
      http.get(`${BASE}/api/v1/repositories`, ({ request }) => {
        url = request.url
        return HttpResponse.json({ items: [], next: null })
      }),
    )
    await createApiClient({ baseUrl: BASE }).GET('/api/v1/repositories', {
      params: { query: { include: ['stats'], limit: 5 } },
    })
    expect(new URL(url).searchParams.get('include')).toBe('stats')
    expect(new URL(url).searchParams.get('limit')).toBe('5')
  })
})

describe('unwrap', () => {
  it('returns the body and rejects with an ApiError on failure', async () => {
    server.use(
      http.get(`${BASE}/api/v1/me`, () => HttpResponse.json({ authenticated: false })),
      http.get(`${BASE}/api/v1/system/status`, () =>
        HttpResponse.json(
          { error: { code: 'forbidden', message: 'Access denied', request_id: 'r1' } },
          { status: 403 },
        ),
      ),
    )
    const client = createApiClient({ baseUrl: BASE })
    await expect(unwrap(client.GET('/api/v1/me'))).resolves.toMatchObject({ authenticated: false })
    const failure = await unwrap(client.GET('/api/v1/system/status')).catch((e: unknown) => e)
    expect(failure).toBeInstanceOf(ApiError)
    expect((failure as ApiError).code).toBe('forbidden')
    expect((failure as ApiError).kind).toBe('forbidden')
  })

  it('keeps the ETag with the body', async () => {
    server.use(
      http.get(`${BASE}/api/v1/repositories/maven`, () =>
        HttpResponse.json({ name: 'maven' }, { headers: { ETag: '"v7"' } }),
      ),
    )
    const result = await unwrapVersioned(
      createApiClient({ baseUrl: BASE }).GET('/api/v1/repositories/{name}', {
        params: { path: { name: 'maven' } },
      }),
    )
    expect(result.etag).toBe('"v7"')
    expect(result.data).toMatchObject({ name: 'maven' })
  })

  it('resolves undefined for a 204', async () => {
    server.use(
      http.post(
        `${BASE}/api/v1/me/actions/sign-out-everywhere`,
        () => new HttpResponse(null, { status: 204 }),
      ),
    )
    const value = await unwrap(
      createApiClient({ baseUrl: BASE }).POST('/api/v1/me/actions/sign-out-everywhere'),
    )
    expect(value).toBeUndefined()
  })
})

describe('cancellation', () => {
  it('rejects a cancelled request with the abort error itself, not an ApiError', async () => {
    server.use(http.get(`${BASE}/api/v1/me`, () => HttpResponse.json({})))
    const controller = new AbortController()
    controller.abort()
    const error = await createApiClient({ baseUrl: BASE })
      .GET('/api/v1/me', { signal: controller.signal })
      .catch((e: unknown) => e)
    expect(error).not.toBeInstanceOf(ApiError)
    expect((error as { name?: string }).name).toBe('AbortError')
  })
})
