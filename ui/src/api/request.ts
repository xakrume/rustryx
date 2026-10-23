import { toApiError } from './errors'

/** A response body together with the ETag the server sent with it (null when it sent none). */
export interface Versioned<T> {
  data: T
  etag: string | null
}

/** What `openapi-fetch` resolves to, reduced to the parts the helpers read. */
interface FetchResult<T> {
  data?: T
  error?: unknown
  response: Response
}

/**
 * Turn an openapi-fetch call into a promise that either resolves with the typed body or
 * rejects with an ApiError. Query functions use this so TanStack Query sees one failure type
 * and its retry rules can read `status` and `retryAfterSecs`.
 *
 *   queryFn: ({ signal }) => unwrap(api.GET('/api/v1/repositories', { signal }))
 *
 * A 204 answer has no body: the result is `undefined`, typed as T because the caller picked
 * an operation that is known to answer with none.
 */
export async function unwrap<T>(call: Promise<FetchResult<T>>): Promise<T> {
  return (await unwrapVersioned(call)).data
}

/**
 * Like `unwrap`, but keeps the ETag. Every read of a mutable object goes through this so the
 * version being edited travels with the data into the `If-Match` header of the update.
 */
export async function unwrapVersioned<T>(call: Promise<FetchResult<T>>): Promise<Versioned<T>> {
  const { data, error, response } = await call
  if (!response.ok) throw toApiError(response, error)
  return { data: data as T, etag: response.headers.get('etag') }
}

/** Request headers for an update guarded by the version that was read. */
export function ifMatch(etag: string | null): Record<string, string> {
  return etag === null ? {} : { 'If-Match': etag }
}
