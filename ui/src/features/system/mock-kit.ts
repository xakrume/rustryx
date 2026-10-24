import { delay } from 'msw'
import { requireSession } from '@/mocks/auth-guard'
import { problem } from '@/mocks/problem'
import { userOf } from '@/mocks/session'
import { LATENCY_MS } from '@/mocks/store'

/**
 * Helpers of the administration mocks (blob stores, cleanup, routing, tasks, system, settings).
 * Imported by `features/<name>/mocks.ts` only, so none of it reaches the production bundle.
 */

/** A UUID in a range of its own, so these fixtures never clash with the shell fixtures. */
export function mockId(kind: number, n: number): string {
  return `0192f6c4-5a1e-7000-8${kind.toString(16).padStart(3, '0')}-${n.toString(16).padStart(12, '0')}`
}

const iso = (ms: number): string => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')
export const hoursAgo = (hours: number): string => iso(Date.now() - hours * 3_600_000)
export const daysAgo = (days: number): string => iso(Date.now() - days * 86_400_000)
export const minutesFromNow = (minutes: number): string => iso(Date.now() + minutes * 60_000)
export const secondsAgo = (seconds: number): string => iso(Date.now() - seconds * 1000)

interface InjectedFault {
  /** Substring of the request path, for example `/blob-stores`. */
  match: string
  status: number
  code: string
  message?: string
  retryAfterSecs?: number
  /** Only for this method; any method when absent. */
  method?: string
}

const FAULT_KEY = 'ryx-mock-faults'

/**
 * Error states for the browser tests: `sessionStorage['ryx-mock-faults']` holds a JSON list of
 * `{ match, status, code, retryAfterSecs?, method? }`. A request whose path contains `match`
 * gets that error instead of its normal answer. Unit tests use MSW overrides instead.
 */
function injectedFault(request: Request): Response | null {
  try {
    const raw = window.sessionStorage.getItem(FAULT_KEY)
    if (raw === null) return null
    const faults = JSON.parse(raw) as InjectedFault[]
    const path = new URL(request.url).pathname
    const hit = faults.find(
      (f) => path.includes(f.match) && (f.method === undefined || f.method === request.method),
    )
    if (hit === undefined) return null
    return problem(hit.status, hit.code, hit.message ?? 'Injected failure for testing', {
      retryAfterSecs: hit.retryAfterSecs,
    })
  } catch {
    return null
  }
}

/**
 * What every administration endpoint does before its own logic: the mock latency, the session
 * (401), the CSRF token on unsafe methods (403), the administrator check (403 `forbidden` for
 * the developer account) and a fault injected by a browser test.
 */
export async function gate(request: Request): Promise<Response | null> {
  await delay(LATENCY_MS)
  const session = requireSession(request)
  if (session instanceof Response) return session
  if (userOf(session)?.admin !== true) {
    return problem(403, 'forbidden', 'You do not have access to this operation')
  }
  return injectedFault(request)
}

export function etagOf(kind: string, id: string, version: number): string {
  return `"${kind}-${id}-v${version}"`
}

/**
 * The contract of `PUT` and `PATCH` (docs/api-conventions.md, section 8): no `If-Match` is 428,
 * a different one is 412. The literal `"stale"` always counts as different, which is what the
 * tests use to open the conflict dialog.
 */
export function checkIfMatch(request: Request, current: string): Response | null {
  const given = request.headers.get('if-match')
  if (given === null) {
    return problem(428, 'precondition_required', 'The request needs an If-Match header')
  }
  if (given === '"stale"' || given !== current) {
    return problem(412, 'precondition_failed', 'The object was changed by someone else')
  }
  return null
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

export type PageResult<T> = { items: T[]; next: string | null } | Response

/** Keyset style paging by `limit` and `after`, with the contract's 400 answers. */
export function pageOf<T>(rows: readonly T[], url: URL, maxLimit = 1000): PageResult<T> {
  const requested = Number(url.searchParams.get('limit') ?? 100)
  if (!Number.isInteger(requested) || requested < 1) {
    return problem(400, 'invalid_query', 'limit must be a positive integer')
  }
  const limit = Math.min(requested, maxLimit)
  const offset = readOffset(url.searchParams.get('after'))
  if (offset === null) {
    return problem(400, 'invalid_page_token', 'The page token is not valid for this request')
  }
  return {
    items: rows.slice(offset, offset + limit),
    next: offset + limit < rows.length ? pageToken(offset + limit) : null,
  }
}

export async function readJsonBody<T>(request: Request): Promise<T | null> {
  try {
    return (await request.json()) as T
  } catch {
    return null
  }
}

export function invalidInput(field: string, message: string): Response {
  return problem(400, 'invalid_input', 'The request was not accepted', {
    details: { fields: [{ field, code: 'invalid', message }] },
  })
}
