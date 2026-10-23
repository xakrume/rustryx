import { describe, expect, it } from 'vitest'
import { ApiError, classify, networkError, parseRetryAfter, toApiError } from './errors'

function response(status: number, headers: Record<string, string> = {}): Response {
  return new Response(null, { status, headers })
}

describe('toApiError', () => {
  it('reads code, message, request id and details from the envelope', () => {
    const error = toApiError(response(409, { 'x-request-id': 'req-header' }), {
      error: {
        code: 'resource_in_use',
        message: 'Still assigned',
        request_id: 'req-body',
        details: { dependents: [{ kind: 'repository', id: '1', name: 'npm-proxy' }] },
      },
    })
    expect(error).toBeInstanceOf(ApiError)
    expect(error.code).toBe('resource_in_use')
    expect(error.message).toBe('Still assigned')
    expect(error.status).toBe(409)
    // The header is the source of truth, the body copy is the fallback.
    expect(error.requestId).toBe('req-header')
    expect(error.kind).toBe('conflict')
    expect(error.dependents).toEqual([{ kind: 'repository', id: '1', name: 'npm-proxy' }])
  })

  it('falls back to the body request id when the header is missing', () => {
    const error = toApiError(response(500), {
      error: { code: 'internal_error', message: 'x', request_id: 'abc' },
    })
    expect(error.requestId).toBe('abc')
  })

  it('still yields a usable error for a body that is not the envelope', () => {
    const html = toApiError(response(502), '<html>Bad gateway</html>')
    expect(html.code).toBe('http_502')
    expect(html.message).toBe('The server failed to handle the request')
    expect(html.kind).toBe('server')
    expect(toApiError(response(404), undefined).code).toBe('http_404')
  })

  it('must_not_trust_a_code_that_is_not_the_stable_format', () => {
    const error = toApiError(response(400), { error: { code: 'Bad Code!', message: 'm' } })
    expect(error.code).toBe('http_400')
  })

  it('cuts a message longer than the contract allows', () => {
    const error = toApiError(response(400), {
      error: { code: 'invalid_input', message: 'x'.repeat(500) },
    })
    expect(error.message).toHaveLength(240)
  })

  it('exposes field errors and drops malformed entries', () => {
    const error = toApiError(response(400), {
      error: {
        code: 'invalid_input',
        message: 'm',
        details: {
          fields: [{ field: '/name', code: 'c', message: 'bad' }, { nope: true }, 'text'],
        },
      },
    })
    expect(error.fieldErrors).toEqual([{ field: '/name', code: 'c', message: 'bad' }])
  })

  it('carries Retry-After in seconds and as an absolute time', () => {
    const error = toApiError(response(429, { 'retry-after': '12' }), {
      error: { code: 'rate_limited', message: 'm' },
    })
    expect(error.retryAfterSecs).toBe(12)
    expect(error.retryAt).toBe(error.receivedAt + 12_000)
  })
})

describe('parseRetryAfter', () => {
  it('accepts whole seconds', () => {
    expect(parseRetryAfter('30')).toBe(30)
    expect(parseRetryAfter(' 7 ')).toBe(7)
  })
  it('converts an HTTP date relative to now', () => {
    const now = Date.parse('2026-10-24T12:00:00Z')
    expect(parseRetryAfter('Sat, 24 Oct 2026 12:01:00 GMT', now)).toBe(60)
  })
  it('treats zero, negative, garbage and absent values as no hint', () => {
    for (const value of ['0', '-5', 'soon', '', null]) expect(parseRetryAfter(value)).toBeNull()
  })
  it('must_not_park_the_ui_for_more_than_a_day', () => {
    expect(parseRetryAfter('999999999')).toBe(86_400)
  })
})

describe('classify', () => {
  it('maps status classes and the special 403', () => {
    expect(classify(0, 'network_error')).toBe('network')
    expect(classify(401, 'token_expired')).toBe('unauthenticated')
    expect(classify(403, 'forbidden')).toBe('forbidden')
    expect(classify(403, 'csrf_rejected')).toBe('csrf')
    expect(classify(404, 'repository_not_found')).toBe('not_found')
    expect(classify(412, 'precondition_failed')).toBe('precondition')
    expect(classify(409, 'already_exists')).toBe('conflict')
    expect(classify(429, 'rate_limited')).toBe('rate_limited')
    expect(classify(503, 'overloaded')).toBe('unavailable')
    expect(classify(500, 'internal_error')).toBe('server')
    // An unknown code is handled by its status class.
    expect(classify(400, 'brand_new_code')).toBe('client')
  })

  it('marks a transport failure as network', () => {
    const error = networkError(new TypeError('Failed to fetch'))
    expect(error.isNetwork).toBe(true)
    expect(error.status).toBe(0)
    expect(error.kind).toBe('network')
  })
})
