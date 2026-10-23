import { problem } from './problem'
import { readSession, type MockSession } from './session'

const UNSAFE = new Set(['POST', 'PUT', 'PATCH', 'DELETE'])

/**
 * What the real server does before a handler runs: no session is 401, a cookie session without
 * the matching X-CSRF-Token on an unsafe method is 403 `csrf_rejected`. The mock enforces both
 * so the UI's CSRF handling and its sign-in redirect are exercised for real, offline.
 */
export function requireSession(request: Request): MockSession | Response {
  const session = readSession()
  if (session === null) return problem(401, 'unauthenticated', 'Authentication is required')
  if (UNSAFE.has(request.method) && request.headers.get('x-csrf-token') !== session.csrf) {
    return problem(403, 'csrf_rejected', 'The CSRF token is missing or does not match')
  }
  return session
}
