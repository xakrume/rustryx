import { describe, expect, it } from 'vitest'
import { ApiError } from '@/api/errors'
import { describeCallbackError, describeLoginFailure } from './login-errors'

const failure = (status: number, code: string, retryAfterSecs?: number) =>
  describeLoginFailure(
    new ApiError({ code, message: 'server text', status, requestId: 'req-1', retryAfterSecs }),
  )

describe('describeLoginFailure', () => {
  it('gives wrong credentials one message that never says which part was wrong', () => {
    expect(failure(401, 'invalid_credentials')).toMatchObject({
      message: 'The username or password is not correct.',
      code: 'invalid_credentials',
      requestId: 'req-1',
      retryAt: null,
    })
  })

  it('turns a lock-out into a countdown target', () => {
    const locked = failure(429, 'locked_out', 30)
    expect(locked.message).toContain('30 s')
    expect(locked.retryAt).not.toBeNull()
    // An unnamed 429 is treated the same way, because the wait is what the person needs.
    expect(failure(429, 'rate_limited', 5).retryAt).not.toBeNull()
  })

  it('names an unreachable login service and identity provider', () => {
    expect(failure(503, 'login_unavailable').message).toMatch(/temporarily unavailable/)
    expect(failure(503, 'something_else').message).toMatch(/temporarily unavailable/)
    expect(failure(503, 'identity_provider_unavailable').message).toMatch(/identity provider/)
  })

  it('handles a network failure, an unknown error and a thing that is not an API error', () => {
    const network = describeLoginFailure(
      new ApiError({ code: 'network_error', message: 'x', status: 0 }),
    )
    expect(network.code).toBeNull()
    expect(network.message).toMatch(/connection/i)
    expect(failure(418, 'teapot').message).toBe('Sign in failed.')
    expect(describeLoginFailure(new Error('boom'))).toMatchObject({
      message: 'Sign in failed.',
      code: null,
      requestId: null,
    })
  })
})

describe('describeCallbackError', () => {
  it('explains every code the SSO callback can put in the address', () => {
    expect(describeCallbackError('oidc_callback_invalid')).toMatch(/expired or was invalid/)
    expect(describeCallbackError('invalid_credentials')).toMatch(/not correct/)
    expect(describeCallbackError('identity_provider_unavailable')).toMatch(/not reachable/)
    expect(describeCallbackError('forbidden')).toMatch(/no role/)
    expect(describeCallbackError('username_conflict')).toMatch(/another sign-in source/)
    expect(describeCallbackError('anything')).toBe('Sign in failed.')
  })
})
