/**
 * The CSRF token of the cookie session (docs/api-conventions.md, 11.3). The server hands it out
 * in the `Session` body and as the script readable cookie `__Host-rustryx_csrf`; the SPA sends
 * it back as `X-CSRF-Token` on POST, PUT, PATCH and DELETE. It is a signed value bound to the
 * session, not a credential, but it still lives only in memory: nothing is written to storage.
 */
const CSRF_COOKIE = '__Host-rustryx_csrf'

let token: string | null = null

export function setCsrfToken(value: string | null): void {
  token = value
}

function readCookie(name: string): string | null {
  if (typeof document === 'undefined') return null
  for (const part of document.cookie.split(';')) {
    const [key, ...rest] = part.trim().split('=')
    if (key === name) return decodeURIComponent(rest.join('='))
  }
  return null
}

/** Memory first (set from /auth/session), then the cookie as a fallback after a reload race. */
export function getCsrfToken(): string | null {
  return token ?? readCookie(CSRF_COOKIE)
}

const UNSAFE = new Set(['POST', 'PUT', 'PATCH', 'DELETE'])

export function isUnsafeMethod(method: string): boolean {
  return UNSAFE.has(method.toUpperCase())
}
