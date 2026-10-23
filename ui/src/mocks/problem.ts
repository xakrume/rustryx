import { HttpResponse } from 'msw'

let counter = 0

export function nextRequestId(): string {
  counter += 1
  return `0192f6c4-5a1e-7000-8000-${counter.toString(16).padStart(12, '0')}`
}

export interface ProblemOptions {
  retryAfterSecs?: number
  details?: Record<string, unknown>
  headers?: Record<string, string>
}

/** The error envelope of docs/api-conventions.md, 5.1, with X-Request-Id and Retry-After. */
export function problem(
  status: number,
  code: string,
  message: string,
  options: ProblemOptions = {},
): Response {
  const requestId = nextRequestId()
  const headers: Record<string, string> = { 'X-Request-Id': requestId, ...options.headers }
  if (options.retryAfterSecs !== undefined) headers['Retry-After'] = String(options.retryAfterSecs)
  const error: Record<string, unknown> = { code, message, request_id: requestId }
  if (options.details) error.details = options.details
  return HttpResponse.json({ error }, { status, headers })
}
