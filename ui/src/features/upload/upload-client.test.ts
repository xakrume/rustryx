import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { apiEvents, type ApiEvent } from '@/api/events'
import { setCsrfToken } from '@/api/csrf'
import { FakeXhr } from '@/test/fake-xhr'
import { newIdempotencyKey, startUpload, type UploadRequest } from './upload-client'

const request: UploadRequest = {
  repository: 'maven releases',
  attributes: { fields: { group_id: 'com.example' }, assets: [{ fields: {}, sha256: null }] },
  files: [new File(['abc'], 'lib-1.0.jar')],
  idempotencyKey: 'ui-key-12345',
}

beforeEach(() => {
  FakeXhr.instances = []
  vi.stubGlobal('XMLHttpRequest', FakeXhr)
  setCsrfToken('csrf-1')
})
afterEach(() => {
  vi.unstubAllGlobals()
  setCsrfToken(null)
  apiEvents.reset()
})

const last = () => FakeXhr.instances.at(-1)!

describe('upload client', () => {
  it('sends the multipart form with the session credentials, CSRF token and idempotency key', async () => {
    const progress: [number, number][] = []
    const handle = startUpload(request, (loaded, total) => progress.push([loaded, total]))
    const xhr = last()
    expect(xhr.method).toBe('POST')
    expect(xhr.url).toBe('/api/v1/components?repository=maven%20releases')
    expect(xhr.withCredentials).toBe(true)
    expect(xhr.headers['X-CSRF-Token']).toBe('csrf-1')
    expect(xhr.headers['Idempotency-Key']).toBe('ui-key-12345')
    const form = xhr.body as FormData
    expect(form.get('attributes')).toBeInstanceOf(Blob)
    expect(form.getAll('file')).toHaveLength(1)
    xhr.upload.onprogress?.({ lengthComputable: true, loaded: 5, total: 10 })
    xhr.upload.onprogress?.({ lengthComputable: false, loaded: 7, total: 0 })
    xhr.finish(201, { component: { id: 'c' }, assets: [], replaced: [] })
    await expect(handle.promise).resolves.toMatchObject({ component: { id: 'c' } })
    expect(progress).toEqual([[5, 10]])
  })

  it('turns an error answer into an ApiError with code, request id and Retry-After, and tells the app', async () => {
    const events: ApiEvent[] = []
    apiEvents.subscribe((e) => events.push(e))
    const handle = startUpload(request, () => undefined)
    last().finish(
      503,
      { error: { code: 'blob_store_unavailable', message: 'down', request_id: 'r-1' } },
      'X-Request-Id: r-1\r\nRetry-After: 7\r\n',
    )
    await expect(handle.promise).rejects.toMatchObject({
      code: 'blob_store_unavailable',
      status: 503,
      requestId: 'r-1',
      retryAfterSecs: 7,
    })
    expect(events).toContainEqual({
      type: 'retry-after',
      status: 503,
      seconds: 7,
      path: '/api/v1/components',
    })
  })

  it('publishes an expired session so the app can send the user to sign in', async () => {
    const events: ApiEvent[] = []
    apiEvents.subscribe((e) => events.push(e))
    const handle = startUpload(request, () => undefined)
    last().finish(401, { error: { code: 'token_expired', message: 'expired' } })
    await expect(handle.promise).rejects.toMatchObject({ status: 401 })
    expect(events.some((e) => e.type === 'unauthenticated')).toBe(true)
  })

  it('reports a lost connection as a network error', async () => {
    const events: ApiEvent[] = []
    apiEvents.subscribe((e) => events.push(e))
    const handle = startUpload(request, () => undefined)
    last().onerror?.()
    await expect(handle.promise).rejects.toMatchObject({ code: 'network_error', status: 0 })
    expect(events).toContainEqual({ type: 'network-failure' })
  })

  it('rejects with an abort error when cancelled', async () => {
    const handle = startUpload(request, () => undefined)
    handle.abort()
    await expect(handle.promise).rejects.toMatchObject({ name: 'AbortError' })
  })

  it('must_repeat_once_with_a_fresh_token_after_csrf_rejected', async () => {
    const fetchCsrf = vi.fn()
    vi.stubGlobal(
      'fetch',
      fetchCsrf.mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify({ csrf_token: 'csrf-2' }), {
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const handle = startUpload(request, () => undefined)
    last().finish(403, { error: { code: 'csrf_rejected', message: 'no' } })
    await vi.waitFor(() => expect(FakeXhr.instances).toHaveLength(2))
    expect(last().headers['X-CSRF-Token']).toBe('csrf-2')
    last().finish(201, { component: { id: 'c' }, assets: [], replaced: [] })
    await expect(handle.promise).resolves.toBeDefined()
    expect(fetchCsrf).toHaveBeenCalledTimes(1)
  })

  it('does not repeat when it was cancelled', async () => {
    const handle = startUpload(request, () => undefined)
    handle.abort()
    await expect(handle.promise).rejects.toMatchObject({ name: 'AbortError' })
    expect(FakeXhr.instances).toHaveLength(1)
  })

  it('makes idempotency keys in the allowed alphabet and length', () => {
    const key = newIdempotencyKey()
    expect(key).toMatch(/^[A-Za-z0-9._:-]{8,128}$/)
    expect(newIdempotencyKey()).not.toBe(key)
  })
})
