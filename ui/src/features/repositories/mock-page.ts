import { problem } from '@/mocks/problem'

/**
 * Keyset paging for the mocks, with the contract's rules (docs/api-conventions.md, section 6):
 * `limit` is clamped to the endpoint maximum and below 1 is 400 `invalid_query`, and `after` is
 * an opaque token that is only valid for the same endpoint. The token here carries an offset;
 * a malformed one is 400 `invalid_page_token`. Returns a Response for those two failures.
 */
export interface PageDefaults {
  limit: number
  max: number
}

export function pageToken(offset: number): string {
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

export function paginate<T>(
  rows: readonly T[],
  url: URL,
  defaults: PageDefaults = { limit: 100, max: 1000 },
): { items: T[]; next: string | null } | Response {
  const raw = url.searchParams.get('limit')
  const requested = raw === null ? defaults.limit : Number(raw)
  if (!Number.isInteger(requested) || requested < 1) {
    return problem(400, 'invalid_query', 'limit must be a positive integer')
  }
  const limit = Math.min(requested, defaults.max)
  const offset = readOffset(url.searchParams.get('after'))
  if (offset === null) {
    return problem(400, 'invalid_page_token', 'The page token is not valid for this request')
  }
  return {
    items: rows.slice(offset, offset + limit),
    next: offset + limit < rows.length ? pageToken(offset + limit) : null,
  }
}
