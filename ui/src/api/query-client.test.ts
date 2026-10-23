import { describe, expect, it } from 'vitest'
import { ApiError } from './errors'
import { MAX_AUTO_RETRY_WAIT_SECS, retryDelay, shouldRetryQuery } from './query-client'

const err = (status: number, retryAfterSecs?: number) =>
  new ApiError({ code: `c${status}`, message: 'm', status, retryAfterSecs })

describe('shouldRetryQuery', () => {
  it('must_not_retry_client_errors', () => {
    for (const status of [400, 401, 403, 404, 409, 412]) {
      expect(shouldRetryQuery(0, err(status))).toBe(false)
    }
  })

  it('retries 429 and 503 up to three times when the wait is short', () => {
    for (const status of [429, 503]) {
      expect(shouldRetryQuery(0, err(status, 5))).toBe(true)
      expect(shouldRetryQuery(2, err(status, 5))).toBe(true)
      expect(shouldRetryQuery(3, err(status, 5))).toBe(false)
    }
  })

  it('must_not_sleep_through_a_long_Retry-After', () => {
    expect(shouldRetryQuery(0, err(429, MAX_AUTO_RETRY_WAIT_SECS + 1))).toBe(false)
    expect(shouldRetryQuery(0, err(429, MAX_AUTO_RETRY_WAIT_SECS))).toBe(true)
  })

  it('retries a transport failure a few times and other 5xx once', () => {
    const offline = new ApiError({ code: 'network_error', message: 'm', status: 0 })
    expect(shouldRetryQuery(2, offline)).toBe(true)
    expect(shouldRetryQuery(3, offline)).toBe(false)
    expect(shouldRetryQuery(0, err(500))).toBe(true)
    expect(shouldRetryQuery(1, err(500))).toBe(false)
  })

  it('retries unknown errors twice', () => {
    expect(shouldRetryQuery(1, new Error('boom'))).toBe(true)
    expect(shouldRetryQuery(2, new Error('boom'))).toBe(false)
  })
})

describe('retryDelay', () => {
  it('waits at least the Retry-After the server asked for', () => {
    const delay = retryDelay(0, err(429, 7))
    expect(delay).toBeGreaterThanOrEqual(7000)
    expect(delay).toBeLessThan(7500)
  })

  it('backs off exponentially, capped at eight seconds plus jitter', () => {
    expect(retryDelay(0, err(500))).toBeGreaterThanOrEqual(1000)
    expect(retryDelay(1, err(500))).toBeGreaterThanOrEqual(2000)
    expect(retryDelay(10, err(500))).toBeLessThan(8400)
  })
})
