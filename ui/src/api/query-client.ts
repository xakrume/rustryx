import { MutationCache, QueryClient } from '@tanstack/react-query'
import { isApiError } from './errors'

/**
 * Stale times by data kind. A query picks one instead of inventing a number, so "how fresh is
 * this page" is answered in one place. Stale-while-revalidate: stale data stays on screen
 * while the refetch runs (docs/ui-page-inventory.md, section 5).
 */
export const STALE_TIME = {
  /** Never changes in a session: formats, upload schemas, settings schema. */
  static: 10 * 60_000,
  /** The signed-in principal and its permissions. */
  principal: 60_000,
  /** Lists and details of administration objects. */
  default: 30_000,
  /** Status, runs, queues: refetched on every mount. */
  live: 0,
} as const

/** A wait longer than this is shown as a countdown the user can act on, not slept through. */
export const MAX_AUTO_RETRY_WAIT_SECS = 60

const MAX_RETRIES_THROTTLED = 3
const MAX_RETRIES_TRANSPORT = 3

/**
 * Which failed queries are tried again. Queries are GETs, so repeating them is safe.
 * - 4xx: never, the caller has to change something; except 429 and 408.
 * - 429 and 503: honour Retry-After, at most three times and only for waits up to a minute.
 * - network failures: a few times with backoff, the banner shows the state meanwhile.
 * - other 5xx: once, a second 500 is a real bug and retries would only hide it.
 * Mutations are never retried by default (see the QueryClient below): a repeated POST can
 * create a second object.
 */
export function shouldRetryQuery(failureCount: number, error: unknown): boolean {
  if (!isApiError(error)) return failureCount < 2
  if (error.isNetwork) return failureCount < MAX_RETRIES_TRANSPORT
  if (error.status === 429 || error.status === 503) {
    const wait = error.retryAfterSecs ?? 1
    return failureCount < MAX_RETRIES_THROTTLED && wait <= MAX_AUTO_RETRY_WAIT_SECS
  }
  if (error.status >= 400 && error.status < 500 && error.status !== 408) return false
  return failureCount < 1
}

/** Retry-After when the server sent one, else exponential backoff with jitter (cap 8 s). */
export function retryDelay(failureCount: number, error: unknown): number {
  if (isApiError(error) && error.retryAfterSecs !== null) {
    return error.retryAfterSecs * 1000 + Math.floor(Math.random() * 400)
  }
  return Math.min(1000 * 2 ** failureCount, 8000) + Math.floor(Math.random() * 400)
}

export interface QueryClientOptions {
  /** Called for a failed mutation that did not set `meta: { silent: true }`. */
  onMutationError?: (error: unknown) => void
}

export function createQueryClient(options: QueryClientOptions = {}): QueryClient {
  return new QueryClient({
    mutationCache: new MutationCache({
      onError: (error, _variables, _context, mutation) => {
        if (mutation.meta?.silent === true) return
        options.onMutationError?.(error)
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: STALE_TIME.default,
        gcTime: 5 * 60_000,
        retry: shouldRetryQuery,
        retryDelay,
        refetchOnWindowFocus: true,
        refetchOnReconnect: true,
      },
      mutations: { retry: false },
    },
  })
}
