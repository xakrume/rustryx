import { delay, http, type HttpHandler } from 'msw'
import { requireSession } from './auth-guard'
import { MOCK_PASSWORD, MOCK_USERS } from './data'
import { generatedHandlers } from './generated-handlers'
import { problem } from './problem'
import { json, LATENCY_MS, visibleRepositories } from './store'
import { meBody, readSession, sessionBody, startSession, writeSession } from './session'
const API = '/api/v1'

async function readBody<T>(request: Request): Promise<T | null> {
  try {
    return (await request.json()) as T
  } catch {
    return null
  }
}

function pageToken(offset: number): string {
  return btoa(JSON.stringify({ o: offset })).replace(/=+$/, '')
}

function readOffset(token: string | null): number | null {
  if (token === null) return 0
  try {
    const parsed = JSON.parse(atob(token)) as { o?: unknown }
    return typeof parsed.o === 'number' && parsed.o >= 0 ? parsed.o : null
  } catch {
    return null
  }
}

/**
 * Hand written handlers: the flows that need state or logic (sign in, session, the principal,
 * the repository list with filters, sort and keyset paging). Special usernames drive the
 * error states of the login page: `locked` answers 429 `locked_out` with Retry-After,
 * `unavailable` answers 503 `login_unavailable`; any other unknown user or a wrong password
 * answers 401 `invalid_credentials`.
 */
const authHandlers: HttpHandler[] = [
  http.get(`${API}/auth/providers`, async () => {
    await delay(LATENCY_MS)
    const ssoDown = window.sessionStorage.getItem('ryx-mock-sso-down') === '1'
    return json({
      local: { enabled: true },
      ldap: { enabled: false },
      oidc: {
        enabled: true,
        available: !ssoDown,
        display_name: 'Keycloak',
        login_path: '/api/v1/auth/oidc/login',
      },
    })
  }),

  http.post(`${API}/auth/login`, async ({ request }) => {
    await delay(LATENCY_MS)
    const body = await readBody<{ username?: unknown; password?: unknown }>(request)
    if (body === null || typeof body.username !== 'string' || typeof body.password !== 'string') {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    if (body.username === 'locked') {
      return problem(429, 'locked_out', 'Too many failed login attempts', { retryAfterSecs: 30 })
    }
    if (body.username === 'unavailable') {
      return problem(503, 'login_unavailable', 'Sign in is temporarily unavailable', {
        retryAfterSecs: 5,
      })
    }
    const user = MOCK_USERS.find((u) => u.username === body.username && u.source === 'local')
    // One answer for unknown user, wrong password and disabled account: no user enumeration.
    if (user === undefined || body.password !== MOCK_PASSWORD) {
      return problem(401, 'invalid_credentials', 'The username or password is not correct')
    }
    return json(sessionBody(startSession(user.username)))
  }),

  http.post(`${API}/auth/logout`, () => {
    writeSession(null)
    return json({ redirect_url: null })
  }),

  http.get(`${API}/auth/session`, async () => {
    await delay(LATENCY_MS)
    return json(sessionBody(readSession()))
  }),

  http.post(`${API}/auth/session/refresh`, ({ request }) => {
    const session = requireSession(request)
    if (session instanceof Response) return session
    const refreshed = { ...session, touchedAt: Date.now() }
    writeSession(refreshed)
    return json(sessionBody(refreshed))
  }),

  http.get(`${API}/auth/csrf`, () => {
    const session = readSession()
    if (session === null) return problem(401, 'unauthenticated', 'Authentication is required')
    return json({ csrf_token: session.csrf })
  }),

  http.get(`${API}/me`, async () => {
    await delay(LATENCY_MS)
    return json(meBody(readSession()))
  }),
]

const dataHandlers: HttpHandler[] = [
  http.get(`${API}/repositories`, async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const q = url.searchParams.get('q')?.toLowerCase()
    const format = url.searchParams.get('format')
    const type = url.searchParams.get('repo_type')
    const online = url.searchParams.get('online')
    const anonymous = url.searchParams.get('allow_anonymous')
    const sort = url.searchParams.get('sort') ?? 'name'
    const withStats = (url.searchParams.get('include') ?? '').split(',').includes('stats')
    const requested = Number(url.searchParams.get('limit') ?? 100)
    if (!Number.isInteger(requested) || requested < 1) {
      return problem(400, 'invalid_query', 'limit must be a positive integer')
    }
    const limit = Math.min(requested, 1000)
    const offset = readOffset(url.searchParams.get('after'))
    if (offset === null) {
      return problem(400, 'invalid_page_token', 'The page token is not valid for this request')
    }

    let rows = visibleRepositories().filter(
      (r) =>
        (q === undefined || r.name.toLowerCase().includes(q)) &&
        (format === null || r.format === format) &&
        (type === null || r.repo_type === type) &&
        (online === null || String(r.online) === online) &&
        (anonymous === null || String(r.allow_anonymous) === anonymous),
    )
    const descending = sort.startsWith('-')
    const key = sort.replace(/^-/, '')
    if (key !== 'name' && key !== 'created_at') {
      return problem(400, 'unsupported_sort', 'The endpoint cannot sort by this field')
    }
    rows = [...rows].sort((a, b) => (a[key] < b[key] ? -1 : a[key] > b[key] ? 1 : 0))
    if (descending) rows.reverse()

    const page = rows.slice(offset, offset + limit)
    const next = offset + limit < rows.length ? pageToken(offset + limit) : null
    return json({
      items: page.map((r) => (withStats ? r : { ...r, stats: null })),
      next,
    })
  }),

  http.get(`${API}/repositories/:name`, async ({ params }) => {
    await delay(LATENCY_MS)
    const repo = visibleRepositories().find((r) => r.name === params.name)
    if (repo === undefined) {
      return problem(404, 'repository_not_found', 'Repository not found')
    }
    return json(repo, { headers: { ETag: `"repo-${repo.id}-${repo.updated_at}"` } })
  }),

  http.get(`${API}/system/status`, ({ request }) => {
    const session = requireSession(request)
    if (session instanceof Response) return session
    return json({
      status: 'ok',
      generated_at: '2026-10-24T09:15:30Z',
      node: 'rustryx-0',
      checks: [],
      readiness: { status: 'ready', checks: [{ name: 'database', status: 'ok' }], advisories: [] },
    })
  }),
]

/**
 * Every feature module may ship its own mock endpoints in src/features/<name>/mocks.ts
 * (`export const handlers: HttpHandler[]`). They are found with import.meta.glob, so adding a
 * feature never means editing this shared file, and two feature branches never conflict here.
 */
const featureModules = import.meta.glob<{ handlers: HttpHandler[] }>('../features/*/mocks.ts', {
  eager: true,
})
const featureHandlers = Object.values(featureModules).flatMap((module) => module.handlers)

/**
 * First match wins, so the order is the priority: a feature may override the core flows, the
 * core flows override the generated examples, and the generated examples answer the rest.
 */
export const handlers: HttpHandler[] = [
  ...featureHandlers,
  ...authHandlers,
  ...dataHandlers,
  ...generatedHandlers(),
]
