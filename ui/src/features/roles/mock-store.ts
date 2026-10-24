import type { components } from '@/api/schema'
import { compile } from '@/features/selectors/mock-engine'
import {
  buildIdentityState,
  id,
  ROLE_ID,
  type IdentityState,
  type RoleRecord,
  type UserRecord,
} from './mock-seed'
import { etagOf, invalidInput, MockProblem } from './mock-http'

type S = components['schemas']
export type RoleView = S['RoleView']
export type PrivilegeView = S['PrivilegeView']
export type SelectorView = S['ContentSelectorView']
export type UserView = S['UserView']
export type MappingView = S['IdpGroupMappingView']
export type TokenView = S['ApiTokenView']
type AuditEvent = S['AuditEvent']

/**
 * The state of the identity mock: one in-memory data set that every identity page reads and
 * writes, so the pages agree with each other (a role deleted on the roles page is gone from the
 * user detail, a user created on the users page shows in the audit log). Replaced, never mutated
 * in place, and re-seeded by `resetIdentity` between tests.
 */
let state: IdentityState = buildIdentityState()

export const identity = (): IdentityState => state
export const resetIdentity = (): void => {
  state = buildIdentityState()
}
export function commit(next: Partial<IdentityState>): void {
  state = { ...state, ...next }
}

export const isoNow = (): string => new Date().toISOString().replace(/\.\d{3}Z$/, 'Z')

let counter = 0
export const freshId = (): string => {
  counter += 1
  return id(0x9000 + counter)
}

// ---------------------------------------------------------------------------------- audit

export interface AuditInput {
  kind: AuditEvent['kind']
  action: string
  target: string | null
  detail?: Record<string, unknown>
  outcome?: 'success' | 'failure'
}

/** Every change a handler makes leaves a record, as the contract demands (`x-audit`). */
export function audit(actor: { userId: string | null; username: string }, event: AuditInput): void {
  const record: AuditEvent = {
    id: freshId(),
    at: isoNow(),
    kind: event.kind,
    actor_id: actor.userId,
    actor_name: actor.username,
    client_ip: '10.1.2.3',
    request_id: `0192f6c4-5a1e-7000-8000-${(0xfeed0000 + counter).toString(16).padStart(12, '0')}`,
    action: event.action,
    target: event.target,
    outcome: event.outcome ?? 'success',
    detail: event.detail ?? {},
  }
  commit({ audit: [record, ...state.audit] })
}

// ---------------------------------------------------------------------------------- roles

const roleRef = (role: RoleRecord): S['RoleRef'] => ({ id: role.id, name: role.name })

export function findRole(roleId: string): RoleRecord {
  const role = state.roles.find((r) => r.id === roleId)
  if (!role) throw new MockProblem(404, 'role_not_found', 'The role does not exist')
  return role
}

const assignedUsers = (roleId: string): number =>
  state.users.filter((u) => u.manualRoleIds.includes(roleId)).length

const mappedGroups = (roleId: string): number =>
  state.mappings.filter((m) => m.role_id === roleId).length

export function roleView(role: RoleRecord): RoleView {
  return {
    ...role,
    privileges: role.privilege_ids.flatMap((pid) => {
      const p = state.privileges.find((x) => x.id === pid)
      return p ? [{ id: p.id, name: p.name, builtin: p.builtin }] : []
    }),
    nested_roles: role.nested_role_ids.flatMap((rid) => {
      const r = state.roles.find((x) => x.id === rid)
      return r ? [roleRef(r)] : []
    }),
    assigned_users: assignedUsers(role.id),
    mapped_groups: mappedGroups(role.id),
  }
}

export const roleWritable = (role: RoleRecord) => ({
  name: role.name,
  description: role.description,
  privilege_ids: role.privilege_ids,
  nested_role_ids: role.nested_role_ids,
})

export const etagOfRole = (role: RoleRecord): string => etagOf(roleWritable(role))

/** Roles reachable from `start` through nesting; cycle safe. */
export function reachableRoles(start: readonly string[]): Set<string> {
  const seen = new Set<string>()
  const queue = [...start]
  while (queue.length > 0) {
    const next = queue.pop()!
    if (seen.has(next)) continue
    seen.add(next)
    queue.push(...(state.roles.find((r) => r.id === next)?.nested_role_ids ?? []))
  }
  return seen
}

export function effectivePrivileges(role: RoleRecord): S['Privilege'][] {
  const roles = reachableRoles([role.id])
  const ids = new Set(state.roles.filter((r) => roles.has(r.id)).flatMap((r) => r.privilege_ids))
  return state.privileges.filter((p) => ids.has(p.id))
}

export interface RoleInput {
  name?: unknown
  description?: unknown
  privilege_ids?: unknown
  nested_role_ids?: unknown
}

export function validateRoleInput(
  input: RoleInput,
  selfId: string | null,
): Omit<RoleRecord, 'id' | 'builtin'> {
  const fields: { field: string; code: string; message: string }[] = []
  const name = typeof input.name === 'string' ? input.name : ''
  if (name.trim() === '' || name.length > 128) {
    fields.push({
      field: '/name',
      code: 'invalid',
      message: 'The name must be 1 to 128 characters.',
    })
  }
  const asIds = (value: unknown): string[] =>
    Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : []
  const privilege_ids = asIds(input.privilege_ids)
  const nested_role_ids = asIds(input.nested_role_ids)
  if (privilege_ids.some((pid) => !state.privileges.some((p) => p.id === pid))) {
    fields.push({
      field: '/privilege_ids',
      code: 'unknown',
      message: 'A selected privilege does not exist.',
    })
  }
  if (nested_role_ids.some((rid) => !state.roles.some((r) => r.id === rid))) {
    fields.push({
      field: '/nested_role_ids',
      code: 'unknown',
      message: 'A nested role does not exist.',
    })
  }
  if (fields.length > 0) throw invalidInput(fields)
  if (state.roles.some((r) => r.id !== selfId && r.name.toLowerCase() === name.toLowerCase())) {
    throw new MockProblem(409, 'already_exists', 'A role with this name exists')
  }
  if (selfId !== null && reachableRoles(nested_role_ids).has(selfId)) {
    throw new MockProblem(409, 'role_cycle', 'The nesting of roles would form a cycle')
  }
  return {
    name,
    description: typeof input.description === 'string' ? input.description : '',
    privilege_ids,
    nested_role_ids,
  }
}

// ----------------------------------------------------------------------------- privileges

export function privilegeView(p: S['Privilege']): PrivilegeView {
  const selector =
    p.grant.type === 'repository-content-selector'
      ? state.selectors.find((s) => s.id === (p.grant as { selector_id: string }).selector_id)
      : undefined
  return {
    ...p,
    roles: state.roles.filter((r) => r.privilege_ids.includes(p.id)).map(roleRef),
    selector: selector ? { id: selector.id, name: selector.name } : null,
  }
}

export const privilegeWritable = (p: S['Privilege']) => ({
  name: p.name,
  description: p.description,
  grant: p.grant,
  actions: p.actions,
})

export function findPrivilege(privilegeId: string): S['Privilege'] {
  const p = state.privileges.find((x) => x.id === privilegeId)
  if (!p) throw new MockProblem(404, 'privilege_not_found', 'The privilege does not exist')
  return p
}

export function validatePrivilegeInput(
  input: Partial<S['PrivilegeInput']>,
  selfId: string | null,
  repositoryNames: readonly string[],
): Omit<S['Privilege'], 'id' | 'builtin'> {
  const fields: { field: string; code: string; message: string }[] = []
  const name = typeof input.name === 'string' ? input.name : ''
  if (name.trim() === '' || name.length > 128) {
    fields.push({
      field: '/name',
      code: 'invalid',
      message: 'The name must be 1 to 128 characters.',
    })
  }
  const actions = Array.isArray(input.actions) ? input.actions : []
  if (actions.length === 0) {
    fields.push({
      field: '/actions',
      code: 'required',
      message: 'Select at least one action: a privilege without actions is refused.',
    })
  }
  const grant = input.grant
  if (!grant || (grant as { type?: string }).type === 'admin') {
    fields.push({
      field: '/grant/type',
      code: 'not_creatable',
      message: 'The admin grant exists only as the built-in privilege.',
    })
  } else {
    const repository = (grant as { repository?: string }).repository ?? ''
    if (repository === '' || (repository !== '*' && !repositoryNames.includes(repository))) {
      fields.push({
        field: '/grant/repository',
        code: 'unknown',
        message: 'The repository does not exist. Use * for all repositories.',
      })
    }
    if (grant.type === 'repository-content-selector') {
      if (!state.selectors.some((s) => s.id === grant.selector_id)) {
        fields.push({
          field: '/grant/selector_id',
          code: 'unknown',
          message: 'Select an existing content selector.',
        })
      }
    }
  }
  if (fields.length > 0) throw invalidInput(fields)
  if (
    state.privileges.some((p) => p.id !== selfId && p.name.toLowerCase() === name.toLowerCase())
  ) {
    throw new MockProblem(409, 'already_exists', 'A privilege with this name exists')
  }
  return {
    name,
    description: typeof input.description === 'string' ? input.description : '',
    grant: grant as S['PrivilegeGrant'],
    actions,
  }
}

// ------------------------------------------------------------------------------ selectors

export function selectorView(s: S['ContentSelector']): SelectorView {
  return {
    ...s,
    used_by_privileges: state.privileges
      .filter((p) => p.grant.type === 'repository-content-selector' && p.grant.selector_id === s.id)
      .map((p) => ({ id: p.id, name: p.name })),
  }
}

export const selectorWritable = (s: S['ContentSelector']) => ({
  name: s.name,
  description: s.description,
  expression: s.expression,
})

export function findSelector(selectorId: string): S['ContentSelector'] {
  const s = state.selectors.find((x) => x.id === selectorId)
  if (!s)
    throw new MockProblem(404, 'content_selector_not_found', 'The content selector does not exist')
  return s
}

export function validateSelectorInput(
  input: Partial<S['ContentSelectorInput']>,
  selfId: string | null,
): Omit<S['ContentSelector'], 'id' | 'updated_at'> {
  const name = typeof input.name === 'string' ? input.name : ''
  const expression = typeof input.expression === 'string' ? input.expression : ''
  if (name.trim() === '' || name.length > 128) {
    throw invalidInput([
      { field: '/name', code: 'invalid', message: 'The name must be 1 to 128 characters.' },
    ])
  }
  const compiled = compile(expression)
  if (Array.isArray(compiled)) {
    throw new MockProblem(400, 'invalid_selector', 'The expression does not compile', {
      details: { compile_errors: compiled },
    })
  }
  if (state.selectors.some((s) => s.id !== selfId && s.name.toLowerCase() === name.toLowerCase())) {
    throw new MockProblem(409, 'already_exists', 'A content selector with this name exists')
  }
  return {
    name,
    description: typeof input.description === 'string' ? input.description : '',
    expression,
  }
}

// --------------------------------------------------------------------------------- users

export function findUser(userId: string): UserRecord {
  const u = state.users.find((x) => x.id === userId)
  if (!u) throw new MockProblem(404, 'user_not_found', 'The user does not exist')
  return u
}

export function userView(u: UserRecord): UserView {
  const external = u.source !== 'local'
  const roleOf = (roleId: string): S['RoleRef'] | null => {
    const r = state.roles.find((x) => x.id === roleId)
    return r ? roleRef(r) : null
  }
  const roles: S['EffectiveRole'][] = [
    ...u.manualRoleIds.flatMap((rid) => {
      const role = roleOf(rid)
      return role ? [{ role, origin: 'manual' as const, groups: [] }] : []
    }),
    ...u.external.flatMap((e) => {
      const role = roleOf(e.roleId)
      return role ? [{ role, origin: e.origin, groups: e.groups }] : []
    }),
  ]
  return {
    id: u.id,
    username: u.username,
    source: u.source,
    issuer: u.issuer,
    subject: u.subject,
    email: u.email,
    display_name: u.display_name,
    enabled: u.enabled,
    created_at: u.created_at,
    last_login_at: u.last_login_at,
    roles,
    groups_seen: u.groupsSeen,
    token_count: state.tokens.filter((t) => t.user_id === u.id && tokenState(t) === 'active')
      .length,
    tokens_valid_after: u.tokensValidAfter,
    editable_fields: external
      ? ['enabled', 'roles']
      : ['display_name', 'email', 'enabled', 'roles', 'password'],
    read_only_reason: external
      ? `Profile fields come from the ${u.source === 'oidc' ? 'identity provider' : 'directory'}.`
      : null,
  }
}

export const userWritable = (u: UserRecord) => ({
  display_name: u.display_name,
  email: u.email,
  enabled: u.enabled,
  roles: u.manualRoleIds,
})

const PASSWORD_PROBLEM = (password: unknown, field: string) => {
  if (typeof password !== 'string' || password.length < 12) {
    return { field, code: 'too_short', message: 'The password needs at least 12 characters.' }
  }
  if (password.trim() === '')
    return { field, code: 'blank', message: 'The password must not be blank.' }
  if (password.startsWith('ryx_')) {
    return { field, code: 'reserved_prefix', message: 'A password must not start with ryx_.' }
  }
  return null
}

export function checkPassword(password: unknown, field: string): void {
  const problem = PASSWORD_PROBLEM(password, field)
  if (problem) throw invalidInput([problem])
}

/** Administrators of the local source, the ones `last_admin` protects. */
export function localAdmins(): UserRecord[] {
  return state.users.filter((u) => u.source === 'local' && u.manualRoleIds.includes(ROLE_ID.admin))
}

export function checkManualRoles(roleIds: unknown, source: UserRecord['source']): string[] {
  const ids = Array.isArray(roleIds)
    ? roleIds.filter((v): v is string => typeof v === 'string')
    : []
  const unknown = ids.find((rid) => !state.roles.some((r) => r.id === rid))
  if (unknown)
    throw invalidInput([{ field: '/role_ids', code: 'unknown', message: 'A role does not exist.' }])
  if (ids.includes(ROLE_ID.anonymous)) {
    throw invalidInput([
      {
        field: '/role_ids',
        code: 'not_assignable',
        message: 'nx-anonymous cannot be assigned to a user.',
      },
    ])
  }
  if (ids.includes(ROLE_ID.admin) && source !== 'local') {
    throw invalidInput([
      {
        field: '/role_ids',
        code: 'not_assignable',
        message:
          'nx-admin can be granted to local accounts only. External accounts get it from the admin groups.',
      },
    ])
  }
  return ids
}

// ---------------------------------------------------------------------------------- tokens

export function tokenState(t: S['ApiToken']): TokenView['state'] {
  if (t.revoked_at !== null) return 'revoked'
  if (t.expires_at !== null && Date.parse(t.expires_at) <= Date.now()) return 'expired'
  return 'active'
}

export function tokenView(t: IdentityState['tokens'][number]): TokenView {
  const owner = state.users.find((u) => u.id === t.user_id)
  return {
    ...t,
    state: tokenState(t),
    owner: {
      id: t.user_id,
      username: owner?.username ?? 'unknown',
      display_name: owner?.display_name ?? null,
      source: owner?.source ?? 'local',
    },
    never_used: t.last_used_at === null,
    project_id: null,
  }
}

// ------------------------------------------------------------------------------------ etags

export const etagOfPrivilege = (p: S['Privilege']): string => etagOf(privilegeWritable(p))
export const etagOfSelector = (s: S['ContentSelector']): string => etagOf(selectorWritable(s))
export const etagOfUser = (u: UserRecord): string => etagOf(userWritable(u))
export const etagOfMapping = (m: S['IdpGroupMapping']): string =>
  etagOf({ role_id: m.role_id, priority: m.priority })
export const etagOfAnonymous = (): string =>
  etagOf({ global_enabled: state.anonymous.globalEnabled })
