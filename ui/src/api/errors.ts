import type { components } from './schema'

export type FieldError = components['schemas']['FieldError']
export type Dependent = components['schemas']['Dependent']

export type ApiErrorKind =
  | 'network'
  | 'unauthenticated'
  | 'csrf'
  | 'forbidden'
  | 'not_found'
  | 'conflict'
  | 'precondition'
  | 'rate_limited'
  | 'unavailable'
  | 'server'
  | 'client'

export interface ApiErrorInit {
  code: string
  message: string
  status: number
  requestId?: string | null
  details?: Readonly<Record<string, unknown>> | null
  retryAfterSecs?: number | null
  cause?: unknown
}

/** The longest message the contract allows; anything longer is not ours and is cut. */
const MAX_MESSAGE = 240

/**
 * A failed API call, parsed from the error envelope `{error:{code,message,request_id,details}}`
 * (docs/api-conventions.md, section 5). Callers switch on `code`, never on `message`; an
 * unknown `code` is handled by `kind`, which follows the status class.
 *
 * `status` 0 means no HTTP answer at all (offline, DNS, connection refused, CORS).
 */
export class ApiError extends Error {
  readonly code: string
  readonly status: number
  readonly requestId: string | null
  readonly details: Readonly<Record<string, unknown>> | null
  readonly retryAfterSecs: number | null
  /** When this error was created, so a Retry-After countdown has a fixed start. */
  readonly receivedAt: number

  constructor(init: ApiErrorInit) {
    super(init.message, init.cause === undefined ? undefined : { cause: init.cause })
    this.name = 'ApiError'
    this.code = init.code
    this.status = init.status
    this.requestId = init.requestId ?? null
    this.details = init.details ?? null
    this.retryAfterSecs = init.retryAfterSecs ?? null
    this.receivedAt = Date.now()
  }

  get kind(): ApiErrorKind {
    return classify(this.status, this.code)
  }

  get isNetwork(): boolean {
    return this.status === 0
  }

  /** Epoch milliseconds when the server said a retry is fine, or null without Retry-After. */
  get retryAt(): number | null {
    return this.retryAfterSecs === null ? null : this.receivedAt + this.retryAfterSecs * 1000
  }

  /** Field level errors (`details.fields`), for forms. Malformed entries are dropped. */
  get fieldErrors(): FieldError[] {
    const raw = this.details?.fields
    if (!Array.isArray(raw)) return []
    return raw.filter(
      (f): f is FieldError =>
        typeof f === 'object' &&
        f !== null &&
        typeof (f as FieldError).field === 'string' &&
        typeof (f as FieldError).message === 'string',
    )
  }

  /** Objects that block the change (`details.dependents`, code `resource_in_use`). */
  get dependents(): Dependent[] {
    const raw = this.details?.dependents
    if (!Array.isArray(raw)) return []
    return raw.filter(
      (d): d is Dependent =>
        typeof d === 'object' && d !== null && typeof (d as Dependent).name === 'string',
    )
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError
}

/** Status class mapping, with the codes that need a different treatment than their status. */
export function classify(status: number, code: string): ApiErrorKind {
  if (status === 0) return 'network'
  if (status === 401) return 'unauthenticated'
  if (status === 403) return code === 'csrf_rejected' ? 'csrf' : 'forbidden'
  if (status === 404) return 'not_found'
  if (status === 412) return 'precondition'
  if (status === 409) return 'conflict'
  if (status === 429) return 'rate_limited'
  if (status === 503) return 'unavailable'
  if (status >= 500) return 'server'
  return 'client'
}

/**
 * Retry-After as whole seconds. The contract sends integers only; an HTTP date is tolerated
 * (proxies in front of the node may rewrite it) and converted. Zero, negative and unparsable
 * values are treated as absent, and the wait is capped at a day so a bad header cannot park
 * the UI for good.
 */
export function parseRetryAfter(header: string | null, now: number = Date.now()): number | null {
  if (header === null) return null
  const trimmed = header.trim()
  let seconds: number
  if (/^\d+$/.test(trimmed)) {
    seconds = Number(trimmed)
  } else {
    const at = Date.parse(trimmed)
    if (Number.isNaN(at)) return null
    seconds = Math.ceil((at - now) / 1000)
  }
  if (!Number.isFinite(seconds) || seconds < 1) return null
  return Math.min(seconds, 86_400)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function fallbackMessage(status: number): string {
  if (status === 0) return 'The server could not be reached'
  if (status === 401) return 'Authentication is required'
  if (status === 403) return 'Access denied'
  if (status === 404) return 'Not found'
  if (status === 429) return 'Too many requests'
  if (status === 503) return 'The service is temporarily unavailable'
  if (status >= 500) return 'The server failed to handle the request'
  return 'The request was not accepted'
}

/**
 * Build an ApiError from a response and its already parsed body. Never throws: a body that is
 * not the envelope (an HTML page from a proxy, an empty 502) still yields a usable error with
 * code `http_<status>`, so the UI never has to special case "the error was not an error".
 */
export function toApiError(response: Response, body: unknown): ApiError {
  const envelope = isRecord(body) && isRecord(body.error) ? body.error : null
  const code =
    envelope && typeof envelope.code === 'string' && /^[a-z][a-z0-9_]*$/.test(envelope.code)
      ? envelope.code
      : `http_${response.status}`
  const message =
    envelope && typeof envelope.message === 'string' && envelope.message.trim() !== ''
      ? envelope.message.slice(0, MAX_MESSAGE)
      : fallbackMessage(response.status)
  const bodyRequestId =
    envelope && typeof envelope.request_id === 'string' ? envelope.request_id : null
  return new ApiError({
    code,
    message,
    status: response.status,
    requestId: response.headers.get('x-request-id') ?? bodyRequestId,
    details: envelope && isRecord(envelope.details) ? envelope.details : null,
    retryAfterSecs: parseRetryAfter(response.headers.get('retry-after')),
  })
}

export function networkError(cause: unknown): ApiError {
  return new ApiError({
    code: 'network_error',
    message: fallbackMessage(0),
    status: 0,
    cause,
  })
}

/**
 * Aborts are cancellations, not failures: they are never turned into an ApiError. Matched by
 * name, not by `instanceof DOMException`, because the error can come from another realm (an
 * iframe, a test environment) where the class is a different object.
 */
export function isAbortError(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    (error as { name?: unknown }).name === 'AbortError'
  )
}
