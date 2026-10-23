import { afterEach, describe, expect, it } from 'vitest'
import { apiEvents } from './events'
import { installServiceStatus, serviceStatus } from './service-status'

let uninstall: (() => void) | null = null
afterEach(() => uninstall?.())

describe('service status', () => {
  it('follows the event bus: down on a network failure, up on any answer', () => {
    uninstall = installServiceStatus()
    apiEvents.emit({ type: 'network-failure' })
    expect(serviceStatus.getSnapshot().backendDown).toBe(true)
    apiEvents.emit({ type: 'network-ok' })
    expect(serviceStatus.getSnapshot().backendDown).toBe(false)
  })

  it('records how long the server asked clients to wait', () => {
    uninstall = installServiceStatus()
    const before = Date.now()
    apiEvents.emit({ type: 'retry-after', status: 503, seconds: 10, path: '/api/v1/me' })
    const { retryUntil, retryStatus } = serviceStatus.getSnapshot()
    expect(retryStatus).toBe(503)
    expect(retryUntil).toBeGreaterThanOrEqual(before + 10_000)
    serviceStatus.clearRetry()
    expect(serviceStatus.getSnapshot().retryUntil).toBeNull()
  })

  it('does not notify when nothing changed', () => {
    uninstall = installServiceStatus()
    let notified = 0
    const stop = serviceStatus.subscribe(() => (notified += 1))
    apiEvents.emit({ type: 'network-ok' })
    apiEvents.emit({ type: 'network-ok' })
    stop()
    expect(notified).toBe(0)
  })

  it('keeps delivering to other subscribers when one of them throws', async () => {
    const seen: string[] = []
    apiEvents.subscribe(() => {
      throw new Error('bad subscriber')
    })
    apiEvents.subscribe((e) => seen.push(e.type))
    const unhandled: unknown[] = []
    const onError = (e: unknown) => unhandled.push(e)
    process.on('uncaughtException', onError)
    apiEvents.emit({ type: 'network-ok' })
    await new Promise((r) => setTimeout(r, 0))
    process.off('uncaughtException', onError)
    expect(seen).toEqual(['network-ok'])
  })
})
