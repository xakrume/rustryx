import { http, type HttpHandler } from 'msw'
import {
  checkOptionalIfMatch,
  handle,
  jsonWithEtag,
  MockProblem,
  noContent,
  pageResponse,
  readJson,
  requireIfMatch,
  slice,
  startsWithText,
  invalidInput,
} from '@/features/roles/mock-http'
import { ROLE_ID, type UserRecord } from '@/features/roles/mock-seed'
import {
  audit,
  checkManualRoles,
  checkPassword,
  commit,
  etagOfUser,
  findUser,
  freshId,
  identity,
  isoNow,
  localAdmins,
  userView,
} from '@/features/roles/mock-store'

const replaceUser = (next: UserRecord): void =>
  commit({ users: identity().users.map((u) => (u.id === next.id ? next : u)) })

const roleNamesOf = (user: UserRecord): string[] => {
  const ids = [...user.manualRoleIds, ...user.external.map((e) => e.roleId)]
  return identity()
    .roles.filter((r) => ids.includes(r.id))
    .map((r) => r.name)
}

/** Stateful mock of the users API: sources, external accounts, last admin and self guards. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/users', ({ request }) =>
    handle(request, { feature: 'users' }, () => {
      const url = new URL(request.url)
      const q = url.searchParams.get('q')
      const source = url.searchParams.get('source')
      const enabled = url.searchParams.get('enabled')
      const role = url.searchParams.get('role')
      const sort = url.searchParams.get('sort') ?? 'username'
      if (!['username', '-username', 'created_at', '-created_at'].includes(sort)) {
        throw new MockProblem(400, 'unsupported_sort', 'The endpoint cannot sort by this field')
      }
      const key = sort.replace(/^-/, '') as 'username' | 'created_at'
      const users = identity()
        .users.filter(
          (u) =>
            q === null ||
            startsWithText(u.username, q) ||
            startsWithText(u.email, q) ||
            startsWithText(u.display_name, q),
        )
        .filter((u) => source === null || u.source === source)
        .filter((u) => enabled === null || u.enabled === (enabled === 'true'))
        .filter((u) => role === null || roleNamesOf(u).includes(role))
        .sort((a, b) => a[key].localeCompare(b[key]))
      if (sort.startsWith('-')) users.reverse()
      return pageResponse(slice(users.map(userView), url))
    }),
  ),

  http.post('/api/v1/users', ({ request }) =>
    handle(request, { feature: 'users' }, async (caller) => {
      const body = await readJson<{
        username?: unknown
        display_name?: unknown
        email?: unknown
        password?: unknown
        role_ids?: unknown
      }>(request)
      const username = typeof body.username === 'string' ? body.username : ''
      if (!/^[A-Za-z0-9][A-Za-z0-9._@-]{0,127}$/.test(username)) {
        throw invalidInput([
          {
            field: '/username',
            code: 'invalid',
            message: 'The username has unsupported characters.',
          },
        ])
      }
      checkPassword(body.password, '/password')
      const roleIds = checkManualRoles(body.role_ids ?? [], 'local')
      const clash = identity().users.find(
        (u) => u.username.toLowerCase() === username.toLowerCase(),
      )
      if (clash) {
        throw new MockProblem(
          409,
          clash.source === 'local' ? 'already_exists' : 'username_conflict',
          clash.source === 'local'
            ? 'A user with this name exists'
            : `The username is used by an account from another source (${clash.source})`,
        )
      }
      const user: UserRecord = {
        id: freshId(),
        username,
        source: 'local',
        issuer: null,
        subject: null,
        email: typeof body.email === 'string' ? body.email : null,
        display_name: typeof body.display_name === 'string' ? body.display_name : null,
        enabled: true,
        created_at: isoNow(),
        last_login_at: null,
        manualRoleIds: roleIds,
        external: [],
        groupsSeen: null,
        tokensValidAfter: null,
        password: String(body.password),
      }
      commit({ users: [...identity().users, user] })
      audit(caller, { kind: 'rbac_change', action: 'user.create', target: username })
      return jsonWithEtag(userView(user), etagOfUser(user), 201)
    }),
  ),

  http.get('/api/v1/users/:userId', ({ request, params }) =>
    handle(request, { feature: 'users' }, () => {
      const user = findUser(String(params.userId))
      return jsonWithEtag(userView(user), etagOfUser(user))
    }),
  ),

  http.patch('/api/v1/users/:userId', ({ request, params }) =>
    handle(request, { feature: 'users' }, async (caller) => {
      const user = findUser(String(params.userId))
      requireIfMatch(request, etagOfUser(user))
      const body = await readJson<{ display_name?: string; email?: string; enabled?: boolean }>(
        request,
      )
      if (user.source !== 'local' && ('display_name' in body || 'email' in body)) {
        throw new MockProblem(
          409,
          'external_identity',
          'This field belongs to the identity provider and cannot be changed here',
        )
      }
      if (body.enabled === false && user.username === caller.username) {
        throw new MockProblem(
          409,
          'cannot_modify_self',
          'You cannot disable the account you are using',
        )
      }
      const next: UserRecord = {
        ...user,
        display_name: body.display_name ?? user.display_name,
        email: body.email ?? user.email,
        enabled: body.enabled ?? user.enabled,
        // Disabling ends the sessions of the account.
        tokensValidAfter: body.enabled === false ? isoNow() : user.tokensValidAfter,
      }
      replaceUser(next)
      audit(caller, {
        kind: 'rbac_change',
        action: 'user.update',
        target: user.username,
        detail: body.enabled === undefined ? {} : { enabled: body.enabled },
      })
      return jsonWithEtag(userView(next), etagOfUser(next))
    }),
  ),

  http.delete('/api/v1/users/:userId', ({ request, params }) =>
    handle(request, { feature: 'users' }, (caller) => {
      const user = findUser(String(params.userId))
      checkOptionalIfMatch(request, etagOfUser(user))
      if (user.username === caller.username) {
        throw new MockProblem(
          409,
          'cannot_modify_self',
          'You cannot delete the account you are using',
        )
      }
      if (
        user.manualRoleIds.includes(ROLE_ID.admin) &&
        user.source === 'local' &&
        localAdmins().length <= 1
      ) {
        throw new MockProblem(409, 'last_admin', 'This is the last local administrator')
      }
      commit({
        users: identity().users.filter((u) => u.id !== user.id),
        tokens: identity().tokens.filter((t) => t.user_id !== user.id),
      })
      audit(caller, { kind: 'rbac_change', action: 'user.delete', target: user.username })
      return noContent()
    }),
  ),

  http.put('/api/v1/users/:userId/roles', ({ request, params }) =>
    handle(request, { feature: 'users' }, async (caller) => {
      const user = findUser(String(params.userId))
      requireIfMatch(request, etagOfUser(user))
      const body = await readJson<{ role_ids?: unknown }>(request)
      const roleIds = checkManualRoles(body.role_ids, user.source)
      const losesAdmin =
        user.manualRoleIds.includes(ROLE_ID.admin) && !roleIds.includes(ROLE_ID.admin)
      if (losesAdmin && user.username === caller.username) {
        throw new MockProblem(
          409,
          'cannot_modify_self',
          'You cannot demote the account you are using',
        )
      }
      if (losesAdmin && user.source === 'local' && localAdmins().length <= 1) {
        throw new MockProblem(409, 'last_admin', 'This is the last local administrator')
      }
      const next = { ...user, manualRoleIds: roleIds }
      replaceUser(next)
      audit(caller, {
        kind: 'rbac_change',
        action: 'user.roles_set',
        target: user.username,
        detail: { role_ids: roleIds },
      })
      return jsonWithEtag(userView(next), etagOfUser(next))
    }),
  ),

  http.post('/api/v1/users/:userId/actions/reset-password', ({ request, params }) =>
    handle(request, { feature: 'users' }, async (caller) => {
      const user = findUser(String(params.userId))
      if (user.source !== 'local') {
        throw new MockProblem(
          409,
          'external_identity',
          'The password belongs to the identity provider',
        )
      }
      const body = await readJson<{ password?: unknown }>(request)
      checkPassword(body.password, '/password')
      replaceUser({ ...user, password: String(body.password), tokensValidAfter: isoNow() })
      audit(caller, { kind: 'rbac_change', action: 'user.password_reset', target: user.username })
      return noContent()
    }),
  ),

  http.post('/api/v1/users/:userId/actions/sign-out', ({ request, params }) =>
    handle(request, { feature: 'users' }, (caller) => {
      const user = findUser(String(params.userId))
      replaceUser({ ...user, tokensValidAfter: isoNow() })
      audit(caller, { kind: 'token_change', action: 'session.revoke_all', target: user.username })
      return noContent()
    }),
  ),
]
