import { delay, HttpResponse } from 'msw'
import { requireSession } from '@/mocks/auth-guard'
import { problem, type ProblemOptions } from '@/mocks/problem'
import { readSession, userOf, type MockSession } from '@/mocks/session'
import { LATENCY_MS } from '@/mocks/store'

/**
 * Helpers shared by the stateful mock handlers of the identity and security pages (roles,
 * privileges, selectors, users, group mappings, tokens, anonymous access, audit). They keep the
 * contract rules in one place so every handler answers the way the real server will: no session
 * is 401, a cookie session without the CSRF header is 403, administration needs `nx-admin`
 * (403 `forbidden`), `PUT` and `PATCH` need a matching `If-Match` (428 and 412), keyset pages
 * carry a token that is bound to the filters.
 */

/** A refusal a handler wants to send; thrown anywhere below `handle`, turned into the envelope. */
export class MockProblem extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly options: ProblemOptions = {},
  ) {
    super(message)
    this.name = 'MockProblem'
  }
}

export function invalidInput(
  fields: readonly { field: string; code: string; message: string }[],
): MockProblem {
  return new MockProblem(400, 'invalid_input', 'The request has invalid fields', {
    details: { fields },
  })
}

export function toResponse(error: unknown): Response {
  if (error instanceof MockProblem) {
    return problem(error.status, error.code, error.message, error.options)
  }
  return problem(500, 'internal_error', 'The server failed to handle the request')
}

/**
 * `sessionStorage['ryx-mock-chaos']` is a comma separated list of `feature:status` pairs
 * (`users:503,audit:429`). It makes the next calls of a feature fail the way a sick server
 * would, so the loading, error, throttled and forbidden states can be reviewed in the browser
 * and in the screenshots without a second mock. Absent or malformed means no failure.
 */
export function chaosFor(feature: string): MockProblem | null {
  let raw: string | null = null
  try {
    raw = window.sessionStorage.getItem('ryx-mock-chaos')
  } catch {
    return null
  }
  if (raw === null) return null
  const entry = raw
    .split(',')
    .map((part) => part.trim().split(':'))
    .find(([name]) => name === feature)
  const status = Number(entry?.[1])
  if (status === 403) return new MockProblem(403, 'forbidden', 'You are not allowed to do this')
  if (status === 429) {
    return new MockProblem(429, 'rate_limited', 'Too many requests', { retryAfterSecs: 5 })
  }
  if (status === 503) {
    return new MockProblem(503, 'store_unavailable', 'The database is not reachable', {
      retryAfterSecs: 5,
    })
  }
  return null
}

export interface HandleOptions {
  /** The feature name the chaos switch uses. */
  feature: string
  /** False for the pages of any signed in user (`/me/*`); default is administration. */
  admin?: boolean
}

export interface Caller {
  session: MockSession
  /** Id of the acting user, for audit records; null when the session user is unknown. */
  userId: string | null
  username: string
}

/**
 * The common frame of a handler: latency, the chaos switch, the session and CSRF guard, the
 * administrator check, and the conversion of a thrown `MockProblem` into the error envelope.
 */
export async function handle(
  request: Request,
  options: HandleOptions,
  run: (caller: Caller) => Response | Promise<Response>,
): Promise<Response> {
  await delay(LATENCY_MS)
  const guarded = requireSession(request)
  if (guarded instanceof Response) return guarded
  try {
    const failure = chaosFor(options.feature)
    if (failure) throw failure
    const user = userOf(readSession())
    if ((options.admin ?? true) && user?.admin !== true) {
      throw new MockProblem(403, 'forbidden', 'You are not allowed to do this')
    }
    return await run({ session: guarded, userId: user?.id ?? null, username: guarded.username })
  } catch (error) {
    return toResponse(error)
  }
}

/** A short stable tag for an ETag header: equal state, equal tag. Not a security hash. */
export function etagOf(value: unknown): string {
  const text = JSON.stringify(value) ?? ''
  let h = 5381
  for (let i = 0; i < text.length; i += 1) h = (h * 33) ^ text.charCodeAt(i)
  return `"mock-${(h >>> 0).toString(16)}"`
}

/** `PUT` and `PATCH`: no `If-Match` is 428, a different or the literal "stale" one is 412. */
export function requireIfMatch(request: Request, current: string): void {
  const header = request.headers.get('if-match')
  if (header === null) {
    throw new MockProblem(428, 'precondition_required', 'The request needs an If-Match header')
  }
  if (header !== current) {
    throw new MockProblem(
      412,
      'precondition_failed',
      'The object was changed by someone else after it was read',
    )
  }
}

/** `DELETE` accepts `If-Match` but does not need it; when present it is enforced. */
export function checkOptionalIfMatch(request: Request, current: string): void {
  if (request.headers.get('if-match') !== null) requireIfMatch(request, current)
}

export function jsonWithEtag(body: unknown, etag: string, status = 200): Response {
  return HttpResponse.json(body as never, {
    status,
    headers: { ETag: etag, ...(status === 201 ? { Location: '/' } : {}) },
  })
}

export function noContent(): Response {
  return new HttpResponse(null, { status: 204 })
}

export async function readJson<T>(request: Request): Promise<T> {
  try {
    return (await request.json()) as T
  } catch {
    throw new MockProblem(400, 'invalid_json', 'The request body is not valid JSON')
  }
}

export interface PageSlice<T> {
  items: T[]
  next: string | null
}

function signatureOf(url: URL, skip: readonly string[]): string {
  return [...url.searchParams.entries()]
    .filter(([key]) => !skip.includes(key))
    .map(([key, value]) => `${key}=${value}`)
    .sort()
    .join('&')
}

/**
 * Keyset paging over an already filtered and sorted list. The token is opaque to the client and,
 * as in the contract, valid only for the same filters and sort: another one is 400
 * `invalid_page_token`, never a silent restart.
 */
export function slice<T>(
  items: readonly T[],
  url: URL,
  paging: { defaultLimit?: number; maxLimit?: number } = {},
): PageSlice<T> {
  const maxLimit = paging.maxLimit ?? 1000
  const requested = url.searchParams.get('limit')
  const limit =
    requested === null ? (paging.defaultLimit ?? 100) : Math.min(Number(requested), maxLimit)
  if (!Number.isInteger(limit) || limit < 1) {
    throw new MockProblem(400, 'invalid_query', 'limit must be a positive integer')
  }
  const signature = signatureOf(url, ['limit', 'after'])
  let offset = 0
  const after = url.searchParams.get('after')
  if (after !== null) {
    try {
      const parsed = JSON.parse(atob(after)) as { o?: unknown; s?: unknown }
      if (typeof parsed.o !== 'number' || parsed.o < 0 || parsed.s !== signature) throw new Error()
      offset = parsed.o
    } catch {
      throw new MockProblem(400, 'invalid_page_token', 'The page token is not valid here')
    }
  }
  const end = offset + limit
  return {
    items: items.slice(offset, end),
    next: end < items.length ? btoa(JSON.stringify({ o: end, s: signature })) : null,
  }
}

export function pageResponse<T>(page: PageSlice<T>): Response {
  return HttpResponse.json(page as never)
}

/** Compare names the way the API's prefix filters do: case-insensitive. */
export const startsWithText = (value: string | null | undefined, prefix: string): boolean =>
  (value ?? '').toLowerCase().startsWith(prefix.toLowerCase())
