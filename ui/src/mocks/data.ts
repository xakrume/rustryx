import type { components } from '@/api/schema'

type RepositoryView = components['schemas']['RepositoryView']
type RepoFormat = components['schemas']['RepoFormat']
type RepoType = components['schemas']['RepoType']

/**
 * Mock accounts and repositories. Everything is deterministic (fixed instants, a seeded
 * sequence instead of Math.random) so tests, screenshots and bug reports see the same data.
 * These are fixtures for the in-browser mock only: the password below is a test value, not a
 * credential of any real system.
 */
export const MOCK_PASSWORD = 'mock-password'

export interface MockUser {
  id: string
  username: string
  display_name: string
  email: string
  source: 'local' | 'oidc'
  admin: boolean
  roleName: string
  roleId: string
}

export const MOCK_USERS: readonly MockUser[] = [
  {
    id: '0192f6c4-5a1e-7000-8000-000000000001',
    username: 'admin',
    display_name: 'Administrator',
    email: 'admin@example.org',
    source: 'local',
    admin: true,
    roleName: 'nx-admin',
    roleId: '0192f6c4-5a1e-7000-8000-00000000000a',
  },
  {
    id: '0192f6c4-5a1e-7000-8000-000000000005',
    username: 'alice',
    display_name: 'Alice Example',
    email: 'alice@example.org',
    source: 'oidc',
    admin: false,
    roleName: 'nx-developer',
    roleId: '0192f6c4-5a1e-7000-8000-00000000000b',
  },
]

export const FIXED_NOW = '2026-10-24T09:15:30Z'

const FORMATS: readonly { format: RepoFormat; prefix: string }[] = [
  { format: 'maven2', prefix: 'maven' },
  { format: 'npm', prefix: 'npm' },
  { format: 'pypi', prefix: 'pypi' },
  { format: 'docker', prefix: 'docker' },
  { format: 'helm', prefix: 'helm' },
  { format: 'raw', prefix: 'raw' },
  { format: 'pub', prefix: 'pub' },
]

/** A tiny seeded generator: same sequence on every run. */
function sequence(seed: number): () => number {
  let state = seed
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296
    return state / 4294967296
  }
}

function uuid(n: number): string {
  return `0192f6c4-5a1e-7000-8000-${n.toString(16).padStart(12, '0')}`
}

function repository(
  index: number,
  name: string,
  format: RepoFormat,
  type: RepoType,
  next: () => number,
): RepositoryView {
  const online = index % 17 !== 0
  const allowAnonymous = type !== 'hosted' || index % 5 === 0
  const created = new Date(Date.UTC(2026, 5, 1 + (index % 120), 7, 2, 11))
    .toISOString()
    .replace('.000Z', 'Z')
  return {
    id: uuid(0x100 + index),
    name,
    format,
    repo_type: type,
    blob_store_id: uuid(0x29),
    write_policy: type === 'hosted' ? 'allow_once' : 'deny',
    allow_anonymous: allowAnonymous,
    online,
    quota_bytes: null,
    cleanup_policy_ids: [],
    routing_rule_id: null,
    proxy:
      type === 'proxy'
        ? {
            remote_url: `https://upstream.example.org/${format}`,
            auth: null,
            content_max_age_secs: 86400,
            metadata_max_age_secs: 1800,
            negative_cache_ttl_secs: 3600,
            max_concurrent_upstream: 16,
          }
        : null,
    group: type === 'group' ? { members: [name.replace('-group', '-releases')] } : null,
    format_config: {},
    created_at: created,
    updated_at: FIXED_NOW,
    blob_store: { id: uuid(0x29), name: 'default', kind: 'local' },
    url: `https://repo.example.org/repository/${name}/`,
    cleanup_policies: [],
    routing_rule: null,
    stats: {
      component_count: Math.floor(next() * 4000),
      asset_count: Math.floor(next() * 16000),
      size_bytes: Math.floor(next() * 40 * 1024 ** 3),
      as_of: FIXED_NOW,
    },
    health: null,
    permissions: {
      browse: true,
      read: true,
      write: type === 'hosted',
      delete: false,
      admin: false,
    },
    upstream:
      type === 'proxy'
        ? {
            state: online ? 'ok' : 'blocked',
            blocked_until: null,
            last_error_code: online ? null : 'upstream_timeout',
            last_checked_at: FIXED_NOW,
            circuit: online ? 'closed' : 'open',
          }
        : null,
    anonymous_effective: allowAnonymous,
  }
}

/** 28 well known repositories plus 100 numbered team ones: enough for several keyset pages. */
export function buildRepositories(): RepositoryView[] {
  const next = sequence(42)
  const out: RepositoryView[] = []
  let index = 0
  for (const { format, prefix } of FORMATS) {
    out.push(repository((index += 1), `${prefix}-releases`, format, 'hosted', next))
    out.push(repository((index += 1), `${prefix}-snapshots`, format, 'hosted', next))
    out.push(repository((index += 1), `${prefix}-proxy`, format, 'proxy', next))
    out.push(repository((index += 1), `${prefix}-group`, format, 'group', next))
  }
  for (let team = 1; team <= 100; team += 1) {
    const { format, prefix } = FORMATS[team % FORMATS.length] ?? FORMATS[0]!
    const name = `team-${String(team).padStart(3, '0')}-${prefix}`
    out.push(repository((index += 1), name, format, 'hosted', next))
  }
  return out
}
