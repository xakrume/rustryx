import { isApiError, type ApiErrorKind } from '@/api/errors'
import { t } from '@/i18n'

export interface ErrorDescription {
  kind: ApiErrorKind | 'unexpected'
  title: string
  /** The server's message (safe to show by contract) or a local explanation. */
  message: string
  code: string | null
  requestId: string | null
  /** Epoch ms after which a retry is fine, from Retry-After; null when not throttled. */
  retryAt: number | null
}

const TITLES: Record<ApiErrorKind | 'unexpected', Parameters<typeof t>[0]> = {
  network: 'errors.title.network',
  unauthenticated: 'errors.title.unauthenticated',
  csrf: 'errors.title.forbidden',
  forbidden: 'errors.title.forbidden',
  not_found: 'errors.title.notFound',
  conflict: 'errors.title.conflict',
  precondition: 'errors.title.precondition',
  rate_limited: 'errors.title.rateLimited',
  unavailable: 'errors.title.unavailable',
  server: 'errors.title.server',
  client: 'errors.title.client',
  unexpected: 'errors.title.unexpected',
}

/**
 * One place that decides how any thrown value is shown. Unknown `code` values are fine: the
 * title follows the status class, the message is the server's. A non-ApiError (a render
 * bug, a library throw) never leaks its text: the message is generic and the detail belongs
 * in the console and the error boundary.
 */
export function describeError(error: unknown): ErrorDescription {
  if (isApiError(error)) {
    return {
      kind: error.kind,
      title: t(TITLES[error.kind]),
      message: error.isNetwork ? t('errors.detail.network') : error.message,
      code: error.isNetwork ? null : error.code,
      requestId: error.requestId,
      retryAt: error.retryAt,
    }
  }
  return {
    kind: 'unexpected',
    title: t(TITLES.unexpected),
    message: t('errors.detail.unexpected'),
    code: null,
    requestId: null,
    retryAt: null,
  }
}
