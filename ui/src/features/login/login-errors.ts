import { isApiError } from '@/api/errors'
import { t } from '@/i18n'
import { formatDuration } from '@/lib/format'

export interface LoginFailure {
  /** User facing text. Never says whether the account exists (no user enumeration). */
  message: string
  code: string | null
  requestId: string | null
  /** Epoch ms when a locked out user may try again; null for every other failure. */
  retryAt: number | null
}

/**
 * Map a failed sign-in call to what the login page shows. Wrong password, unknown user and
 * disabled account all arrive as `invalid_credentials` and get ONE message on purpose
 * (docs/api-conventions.md, 5.3 and 11.5).
 */
export function describeLoginFailure(error: unknown): LoginFailure {
  if (!isApiError(error)) {
    return { message: t('auth.login.errors.generic'), code: null, requestId: null, retryAt: null }
  }
  const base = { code: error.code, requestId: error.requestId, retryAt: null as number | null }
  switch (error.code) {
    case 'invalid_credentials':
      return { ...base, message: t('auth.login.errors.invalidCredentials') }
    case 'locked_out':
      return {
        ...base,
        message: t('auth.login.errors.lockedOut', {
          time: formatDuration(error.retryAfterSecs ?? 0),
        }),
        retryAt: error.retryAt,
      }
    case 'login_unavailable':
      return { ...base, message: t('auth.login.errors.loginUnavailable') }
    case 'identity_provider_unavailable':
      return { ...base, message: t('auth.login.errors.idpUnavailable') }
    default:
      break
  }
  if (error.isNetwork) {
    return { ...base, code: null, message: t('errors.detail.network') }
  }
  if (error.status === 429) {
    return {
      ...base,
      message: t('auth.login.errors.lockedOut', {
        time: formatDuration(error.retryAfterSecs ?? 0),
      }),
      retryAt: error.retryAt,
    }
  }
  if (error.status === 503) {
    return { ...base, message: t('auth.login.errors.loginUnavailable') }
  }
  return { ...base, message: t('auth.login.errors.generic') }
}

/** Failure codes the OIDC callback puts in `/login?error=<code>` (docs/api-conventions.md, 11.4). */
export function describeCallbackError(code: string): string {
  switch (code) {
    case 'oidc_callback_invalid':
      return t('auth.login.errors.callbackInvalid')
    case 'invalid_credentials':
      return t('auth.login.errors.invalidCredentials')
    case 'identity_provider_unavailable':
      return t('auth.login.errors.idpUnavailable')
    case 'forbidden':
      return t('auth.login.errors.forbidden')
    case 'username_conflict':
      return t('auth.login.errors.usernameConflict')
    default:
      return t('auth.login.errors.generic')
  }
}
