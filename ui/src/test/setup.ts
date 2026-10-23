import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { toHaveNoViolations } from 'jest-axe'
import { afterAll, afterEach, beforeAll, expect } from 'vitest'
import { setCsrfToken } from '@/api/csrf'
import { apiEvents } from '@/api/events'
import { serviceStatus } from '@/api/service-status'
import { toastStore } from '@/lib/toast-store'
import { installMatchMedia, resetMedia } from './media'
import { server } from './server'

expect.extend(toHaveNoViolations)

function memoryStorage(): Storage {
  const map = new Map<string, string>()
  return {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key: string) => map.get(key) ?? null,
    key: (index: number) => [...map.keys()][index] ?? null,
    removeItem: (key: string) => void map.delete(key),
    setItem: (key: string, value: string) => void map.set(key, String(value)),
  }
}

// Newer Node versions define their own `localStorage` global that is unusable without a flag
// (and warns on every access) and can shadow jsdom's. Tests always use an in memory Storage:
// deterministic in every Node version, and a fresh one per test file.
if (typeof window !== 'undefined') {
  for (const name of ['localStorage', 'sessionStorage'] as const) {
    Object.defineProperty(window, name, { value: memoryStorage(), configurable: true })
  }
}

// jsdom lacks these browser APIs that Radix, cmdk and Recharts call.
class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
const hasDom = typeof window !== 'undefined'
if (hasDom) {
  globalThis.ResizeObserver ??= ResizeObserverStub as unknown as typeof ResizeObserver
  Element.prototype.scrollIntoView ??= () => {}
  // jsdom logs "not implemented" for it; the router calls it for scroll restoration.
  window.scrollTo = () => {}
  Element.prototype.hasPointerCapture ??= () => false
  Element.prototype.setPointerCapture ??= () => {}
  Element.prototype.releasePointerCapture ??= () => {}
  installMatchMedia()
}

beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))

afterEach(() => {
  cleanup()
  server.resetHandlers()
  apiEvents.reset()
  serviceStatus.reset()
  toastStore.clear()
  setCsrfToken(null)
  resetMedia()
  if (hasDom) {
    window.localStorage.clear()
    window.sessionStorage.clear()
  }
  if (hasDom) {
    document.documentElement.removeAttribute('data-theme')
    document.documentElement.removeAttribute('data-density')
    document.body.removeAttribute('style')
  }
})

afterAll(() => server.close())
