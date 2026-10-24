import { api } from '@/api/client'
import { getCsrfToken, setCsrfToken } from '@/api/csrf'
import { apiEvents } from '@/api/events'
import { networkError, parseRetryAfter, toApiError, type ApiError } from '@/api/errors'
import type { components } from '@/api/schema'

export type UploadAttributes = components['schemas']['UploadAttributes']
export type UploadResult = components['schemas']['UploadResult']

export interface UploadRequest {
  repository: string
  attributes: UploadAttributes
  files: readonly File[]
  /** 8 to 128 characters of [A-Za-z0-9._:-]; a retry with the same key replays the first result. */
  idempotencyKey: string
}

export interface UploadHandle {
  promise: Promise<UploadResult>
  /** Cancel the request. The promise rejects with an AbortError; the server drops the partial blob. */
  abort: () => void
}

export type ProgressListener = (loaded: number, total: number) => void

export function newIdempotencyKey(): string {
  const random =
    typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`
  return `ui-${random}`
}

function responseHeaders(xhr: XMLHttpRequest): Headers {
  const headers = new Headers()
  for (const line of xhr
    .getAllResponseHeaders()
    .trim()
    .split(/[\r\n]+/)) {
    const index = line.indexOf(':')
    if (index > 0) headers.append(line.slice(0, index).trim(), line.slice(index + 1).trim())
  }
  return headers
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return null
  }
}

function buildForm(request: UploadRequest): FormData {
  const form = new FormData()
  form.append(
    'attributes',
    new Blob([JSON.stringify(request.attributes)], { type: 'application/json' }),
    'attributes.json',
  )
  for (const file of request.files) form.append('file', file, file.name)
  return form
}

/**
 * Upload with `XMLHttpRequest`, not `fetch`: only XHR reports how many bytes of the body were
 * sent, and an upload of gigabytes needs a progress bar and a Cancel button. The body is the
 * multipart form the contract describes (a JSON `attributes` part and the `file` parts), streamed
 * by the browser, never held as one string.
 *
 * It shares the behaviour of the API client: the CSRF header of the cookie session, a repeat after
 * 403 `csrf_rejected` with a fresh token (once; the files are still in hand, so the repeat is
 * cheap here, unlike for a streamed `fetch`), and the events that drive the sign-in redirect and the
 * Retry-After countdown.
 */
export function startUpload(request: UploadRequest, onProgress: ProgressListener): UploadHandle {
  let current: XMLHttpRequest | null = null
  let rejectCurrent: ((reason: unknown) => void) | null = null
  let aborted = false

  const send = (): Promise<UploadResult> =>
    new Promise<UploadResult>((resolve, reject) => {
      const xhr = new XMLHttpRequest()
      current = xhr
      rejectCurrent = reject
      xhr.open('POST', `/api/v1/components?repository=${encodeURIComponent(request.repository)}`)
      xhr.withCredentials = true
      xhr.setRequestHeader('Accept', 'application/json')
      xhr.setRequestHeader('Idempotency-Key', request.idempotencyKey)
      const csrf = getCsrfToken()
      if (csrf !== null) xhr.setRequestHeader('X-CSRF-Token', csrf)
      xhr.upload.onprogress = (event) => {
        if (event.lengthComputable) onProgress(event.loaded, event.total)
      }
      xhr.onload = () => {
        apiEvents.emit({ type: 'network-ok' })
        const body: unknown = parseJson(xhr.responseText)
        if (xhr.status >= 200 && xhr.status < 300) {
          resolve(body as UploadResult)
          return
        }
        const headers = responseHeaders(xhr)
        const error = toApiError(new Response(null, { status: xhr.status, headers }), body)
        if (xhr.status === 401)
          apiEvents.emit({ type: 'unauthenticated', path: '/api/v1/components' })
        const wait = parseRetryAfter(headers.get('retry-after'))
        if ((xhr.status === 429 || xhr.status === 503) && wait !== null) {
          apiEvents.emit({
            type: 'retry-after',
            status: xhr.status,
            seconds: wait,
            path: '/api/v1/components',
          })
        }
        reject(error)
      }
      xhr.onerror = () => {
        apiEvents.emit({ type: 'network-failure' })
        reject(networkError(new Error('upload failed')))
      }
      xhr.onabort = () => reject(new DOMException('The upload was cancelled', 'AbortError'))
      xhr.send(buildForm(request))
    })

  const promise = (async (): Promise<UploadResult> => {
    try {
      return await send()
    } catch (error) {
      const csrfRejected = isCsrfRejected(error)
      if (!csrfRejected || aborted) throw error
      const fresh = await api.GET('/api/v1/auth/csrf')
      if (fresh.data) setCsrfToken(fresh.data.csrf_token)
      return send()
    }
  })()

  return {
    promise,
    abort: () => {
      aborted = true
      current?.abort()
      // A browser fires `abort` and the request rejects by itself; rejecting here as well makes the
      // cancellation certain in an environment that does not (the first rejection wins).
      rejectCurrent?.(new DOMException('The upload was cancelled', 'AbortError'))
    },
  }
}

function isCsrfRejected(error: unknown): error is ApiError {
  return (
    typeof error === 'object' &&
    error !== null &&
    (error as ApiError).code === 'csrf_rejected' &&
    (error as ApiError).status === 403
  )
}
