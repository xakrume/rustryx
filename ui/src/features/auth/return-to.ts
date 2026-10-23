/**
 * The `return_to` of the sign-in flow must be a plain absolute path of this app. The server
 * applies the same rule to its own `return_to` (docs/api-conventions.md, 11.4); the UI checks
 * again because the value arrives from the address bar and `//evil.example` or
 * `/\evil.example` would turn a login redirect into an open redirect.
 */
const SAFE_PATH = /^\/(?![/\\])/

/** Control characters (CR, LF, NUL, ...) would allow header or log injection downstream. */
function hasControlCharacter(value: string): boolean {
  for (let i = 0; i < value.length; i += 1) {
    if (value.charCodeAt(i) < 0x20) return true
  }
  return false
}

export function sanitizeReturnTo(value: string | null | undefined, fallback = '/'): string {
  if (typeof value !== 'string' || value.length > 512) return fallback
  if (!SAFE_PATH.test(value) || hasControlCharacter(value)) return fallback
  // The login page is never a destination: it would bounce a signed-in user in a loop.
  if (value === '/login' || value.startsWith('/login?')) return fallback
  return value
}
