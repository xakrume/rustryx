import type { components } from '@/api/schema'
import { FIXED_NOW, MOCK_USERS, type MockUser } from './data'

type Session = components['schemas']['Session']
type MeView = components['schemas']['MeView']

/**
 * The mock session lives in sessionStorage of the tab. MSW handlers run in the page, so they
 * can read it, and a reload keeps the user signed in the way a real cookie would. This is mock
 * only: the real session is an HttpOnly cookie the browser JavaScript cannot see.
 */
const KEY = 'ryx-mock-session'
const IDLE_MS = 30 * 60_000
const ABSOLUTE_MS = 8 * 3_600_000

export interface MockSession {
  username: string
  csrf: string
  issuedAt: number
  touchedAt: number
}

export function readSession(): MockSession | null {
  try {
    const raw = window.sessionStorage.getItem(KEY)
    if (raw === null) return null
    const parsed = JSON.parse(raw) as MockSession
    const expired =
      Date.now() - parsed.touchedAt > IDLE_MS || Date.now() - parsed.issuedAt > ABSOLUTE_MS
    return expired ? null : parsed
  } catch {
    return null
  }
}

export function writeSession(session: MockSession | null): void {
  try {
    if (session === null) window.sessionStorage.removeItem(KEY)
    else window.sessionStorage.setItem(KEY, JSON.stringify(session))
  } catch {
    // Storage blocked: the mock then behaves as always signed out.
  }
}

export function startSession(username: string): MockSession {
  const now = Date.now()
  const session: MockSession = {
    username,
    csrf: `mock-csrf-${now.toString(36)}-${Math.floor(Math.random() * 1e9).toString(36)}`,
    issuedAt: now,
    touchedAt: now,
  }
  writeSession(session)
  return session
}

export function userOf(session: MockSession | null): MockUser | null {
  if (session === null) return null
  return MOCK_USERS.find((u) => u.username === session.username) ?? null
}

const iso = (ms: number): string => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')

export function sessionBody(session: MockSession | null): Session {
  const user = userOf(session)
  if (session === null || user === null) {
    return {
      authenticated: false,
      user: null,
      auth_method: null,
      csrf_token: null,
      issued_at: null,
      idle_expires_at: null,
      absolute_expires_at: null,
      idle_timeout_secs: 1800,
      absolute_timeout_secs: 28800,
    }
  }
  return {
    authenticated: true,
    user: {
      id: user.id,
      username: user.username,
      display_name: user.display_name,
      source: user.source,
    },
    auth_method: 'internal_jwt',
    csrf_token: session.csrf,
    issued_at: iso(session.issuedAt),
    idle_expires_at: iso(Math.min(session.touchedAt + IDLE_MS, session.issuedAt + ABSOLUTE_MS)),
    absolute_expires_at: iso(session.issuedAt + ABSOLUTE_MS),
    idle_timeout_secs: 1800,
    absolute_timeout_secs: 28800,
  }
}

const ALL_AREAS: MeView['permissions']['admin_areas'] = [
  'users',
  'roles',
  'privileges',
  'selectors',
  'mappings',
  'tokens',
  'blobstores',
  'tasks',
  'settings',
  'audit',
  'system',
]

export function meBody(session: MockSession | null): MeView {
  const user = userOf(session)
  const base = {
    token_policy: { default_ttl_days: 90, max_ttl_days: 365, require_expiry: true },
    system: { read_only: false, anonymous_enabled: true, base_url_configured: true },
    token: null,
    groups: [] as string[],
    groups_confirmed_at: null,
  }
  if (user === null) {
    return {
      ...base,
      authenticated: false,
      user: null,
      auth_method: 'anonymous',
      roles: [],
      permissions: {
        is_admin: false,
        admin_areas: [],
        repository_grants: [
          {
            privilege: 'nx-anonymous',
            repository: '*',
            format: null,
            actions: ['browse', 'read'],
            selector: false,
          },
        ],
        can: {
          create_repository: false,
          upload: false,
          create_personal_token: false,
          view_audit: false,
          administer: false,
        },
      },
    }
  }
  return {
    ...base,
    authenticated: true,
    user: {
      id: user.id,
      username: user.username,
      display_name: user.display_name,
      email: user.email,
      source: user.source,
      issuer: user.source === 'oidc' ? 'https://sso.example.org/realms/rustryx' : null,
      subject: user.source === 'oidc' ? '9f1c2f0e-1d2b-4b53-8f55-0a5f4cbb2c11' : null,
      enabled: true,
      created_at: FIXED_NOW,
    },
    auth_method: 'internal_jwt',
    groups: user.source === 'oidc' ? ['/rustryx/team-payments/publishers'] : [],
    roles: [
      {
        role: { id: user.roleId, name: user.roleName },
        origin: user.source === 'oidc' ? 'oidc' : 'manual',
        groups: user.source === 'oidc' ? ['/rustryx/team-payments/publishers'] : [],
      },
    ],
    permissions: {
      is_admin: user.admin,
      admin_areas: user.admin ? ALL_AREAS : [],
      repository_grants: [
        {
          privilege: 'nx-repository-view-*-*',
          repository: '*',
          format: null,
          actions: user.admin ? ['browse', 'read', 'write', 'delete'] : ['browse', 'read'],
          selector: false,
        },
      ],
      can: {
        create_repository: user.admin,
        upload: user.admin || user.username === 'alice',
        create_personal_token: true,
        view_audit: user.admin,
        administer: user.admin,
      },
    },
  }
}
