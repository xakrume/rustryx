import createClient, { type Client, type Middleware } from 'openapi-fetch'
import { getCsrfToken, isUnsafeMethod, setCsrfToken } from './csrf'
import { apiEvents } from './events'
import { isAbortError, networkError, parseRetryAfter } from './errors'
import type { paths } from './schema'

export type ApiClient = Client<paths>

export interface ApiClientConfig {
  /** Origin and path prefix of the API. Empty means same origin, which production uses. */
  baseUrl?: string
  /** Replaceable for tests. Receives a fully built Request. */
  fetch?: (request: Request) => Promise<Response>
}

/** Sign-in answers 401 for a wrong password: that is a form error, not an expired session. */
const NO_REDIRECT_ON_401 = new Set([
  '/api/v1/auth/login',
  '/api/v1/auth/logout',
  '/api/v1/auth/providers',
  // A wrong CURRENT password counts like a failed login (api-conventions, 11.5): the form shows
  // it at the field. It must not look like an expired session and throw the person to sign in.
  '/api/v1/me/password',
])

const CSRF_PATH = '/api/v1/auth/csrf'

/**
 * Only JSON and bodiless requests may be replayed after a CSRF refresh. Cloning a streamed
 * multipart upload would make the platform buffer the whole body (up to 5 GiB) in memory,
 * so uploads are never cloned; their caller handles `csrf_rejected` itself.
 */
function canReplay(request: Request): boolean {
  const type = request.headers.get('content-type') ?? ''
  return request.body === null || type.startsWith('application/json')
}

async function isCsrfRejected(response: Response): Promise<boolean> {
  if (response.status !== 403) return false
  try {
    const body = (await response.clone().json()) as { error?: { code?: unknown } }
    return body.error?.code === 'csrf_rejected'
  } catch {
    return false
  }
}

async function refreshCsrf(
  send: (request: Request) => Promise<Response>,
  baseUrl: string,
): Promise<string | null> {
  try {
    const response = await send(
      new Request(`${baseUrl}${CSRF_PATH}`, {
        credentials: 'include',
        headers: { Accept: 'application/json' },
      }),
    )
    if (!response.ok) return null
    const body = (await response.json()) as { csrf_token?: unknown }
    const fresh = typeof body.csrf_token === 'string' ? body.csrf_token : null
    setCsrfToken(fresh)
    return fresh
  } catch {
    return null
  }
}

/**
 * Wraps the platform fetch with the behaviour every call shares:
 * - a transport failure becomes an ApiError with status 0 (aborts pass through untouched,
 *   because a cancelled query is not an error), and the banner is told;
 * - 403 `csrf_rejected` on a replayable unsafe request fetches a fresh token and repeats the
 *   request exactly once (docs/api-conventions.md, 5.3);
 * - 401 and Retry-After answers are published on the event bus for the sign-in redirect and
 *   the countdown banner.
 */
function guardedFetch(
  send: (request: Request) => Promise<Response>,
  baseUrl: string,
): (request: Request) => Promise<Response> {
  return async (request) => {
    const replay = isUnsafeMethod(request.method) && canReplay(request) ? request.clone() : null
    let response: Response
    try {
      response = await send(request)
    } catch (error) {
      if (isAbortError(error)) throw error
      apiEvents.emit({ type: 'network-failure' })
      throw networkError(error)
    }
    apiEvents.emit({ type: 'network-ok' })

    if (replay !== null && (await isCsrfRejected(response))) {
      const fresh = await refreshCsrf(send, baseUrl)
      if (fresh !== null) {
        const headers = new Headers(replay.headers)
        headers.set('X-CSRF-Token', fresh)
        try {
          response = await send(new Request(replay, { headers }))
        } catch (error) {
          if (isAbortError(error)) throw error
          apiEvents.emit({ type: 'network-failure' })
          throw networkError(error)
        }
      }
    }

    const path = new URL(request.url, 'http://localhost').pathname
    if (response.status === 401 && !NO_REDIRECT_ON_401.has(path)) {
      apiEvents.emit({ type: 'unauthenticated', path })
    }
    if (response.status === 429 || response.status === 503) {
      const seconds = parseRetryAfter(response.headers.get('retry-after'))
      if (seconds !== null) {
        apiEvents.emit({ type: 'retry-after', status: response.status, seconds, path })
      }
    }
    return response
  }
}

/** Adds the CSRF header to unsafe requests of the cookie session; bearer callers have none. */
const requestHeaders: Middleware = {
  onRequest({ request }) {
    if (isUnsafeMethod(request.method) && !request.headers.has('Authorization')) {
      const token = getCsrfToken()
      if (token !== null) request.headers.set('X-CSRF-Token', token)
    }
    if (!request.headers.has('Accept')) request.headers.set('Accept', 'application/json')
    return request
  },
}

export function createApiClient(config: ApiClientConfig = {}): ApiClient {
  const baseUrl = config.baseUrl ?? ''
  const send = config.fetch ?? ((request: Request) => globalThis.fetch(request))
  const client = createClient<paths>({
    baseUrl,
    // The session is a cookie. With the default same origin base URL this is the browser
    // default anyway; it matters only when VITE_API_BASE_URL points at another origin.
    credentials: 'include',
    fetch: guardedFetch(send, baseUrl),
    // The contract sends lists comma separated (`include=stats,x`, `kind=a,b`), not as repeated
    // keys: OpenAPI style form with explode false (docs/api-conventions.md, section 2).
    querySerializer: { array: { style: 'form', explode: false } },
  })
  client.use(requestHeaders)
  return client
}

/** The application client. Tests build their own with `createApiClient`. */
export const api: ApiClient = createApiClient({
  baseUrl: import.meta.env.VITE_API_BASE_URL ?? '',
})
