import { delay, http, HttpResponse, type HttpHandler } from 'msw'
import { handle, invalidInput, MockProblem, noContent, readJson } from '@/features/roles/mock-http'
import {
  audit,
  checkPassword,
  commit,
  identity,
  isoNow,
  userView,
} from '@/features/roles/mock-store'
import { meBody, readSession, writeSession } from '@/mocks/session'
import { json, LATENCY_MS } from '@/mocks/store'

/**
 * `/me` of the shell mock knows one role per account. This version answers with the roles of the
 * identity store (manual and from groups, each with its origin) and the groups seen at sign in,
 * so the profile page and the user menu show the same, richer data as the user administration.
 */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/me', async () => {
    await delay(LATENCY_MS)
    const session = readSession()
    const base = meBody(session)
    const record = identity().users.find((u) => u.username === session?.username)
    if (!record || base.user === null) return json(base)
    const view = userView(record)
    return json({
      ...base,
      user: { ...base.user, created_at: record.created_at },
      roles: view.roles,
      groups: record.groupsSeen ?? [],
      groups_confirmed_at: record.source === 'local' ? null : isoNow(),
    })
  }),

  http.put('/api/v1/me/password', ({ request }) =>
    handle(request, { feature: 'profile', admin: false }, async (caller) => {
      const body = await readJson<{ current_password?: unknown; new_password?: unknown }>(request)
      const user = identity().users.find((u) => u.id === caller.userId)
      if (!user) throw new MockProblem(404, 'user_not_found', 'The user does not exist')
      if (user.source !== 'local') {
        throw new MockProblem(
          409,
          'external_identity',
          'The password belongs to the identity provider',
        )
      }
      if (typeof body.current_password !== 'string' || body.current_password === '') {
        throw invalidInput([
          { field: '/current_password', code: 'required', message: 'Enter the current password.' },
        ])
      }
      checkPassword(body.new_password, '/new_password')
      // A wrong current password counts like a failed login, so the answer is the login one.
      if (body.current_password !== user.password) {
        throw new MockProblem(401, 'invalid_credentials', 'The current password is not correct')
      }
      commit({
        users: identity().users.map((u) =>
          u.id === user.id
            ? { ...u, password: String(body.new_password), tokensValidAfter: isoNow() }
            : u,
        ),
      })
      audit(caller, { kind: 'rbac_change', action: 'user.password_change', target: user.username })
      return noContent()
    }),
  ),

  http.post('/api/v1/me/actions/sign-out-everywhere', ({ request }) =>
    handle(request, { feature: 'profile', admin: false }, (caller) => {
      audit(caller, { kind: 'token_change', action: 'session.revoke_all', target: caller.username })
      writeSession(null)
      return new HttpResponse(null, { status: 204 })
    }),
  ),
]
