import type { components } from '@/api/schema'

type S = components['schemas']
export type RoleRecord = S['Role']
export type PrivilegeRecord = S['Privilege']
export type SelectorRecord = S['ContentSelector']
export type MappingRecord = S['IdpGroupMapping']
export type AuditRecord = S['AuditEvent']
export type TokenRecord = S['ApiToken'] & { kind: 'personal' | 'robot' | 'service' }

export interface ExternalRole {
  roleId: string
  origin: 'oidc' | 'ldap'
  groups: string[]
}

export interface UserRecord {
  id: string
  username: string
  source: 'local' | 'ldap' | 'oidc'
  issuer: string | null
  subject: string | null
  email: string | null
  display_name: string | null
  enabled: boolean
  created_at: string
  last_login_at: string | null
  manualRoleIds: string[]
  external: ExternalRole[]
  groupsSeen: string[] | null
  tokensValidAfter: string | null
  /** A fixture value of the mock only; the real API never returns or stores a clear password. */
  password: string | null
}

export interface IdentityState {
  roles: RoleRecord[]
  privileges: PrivilegeRecord[]
  selectors: SelectorRecord[]
  users: UserRecord[]
  mappings: MappingRecord[]
  tokens: TokenRecord[]
  audit: AuditRecord[]
  anonymous: { globalEnabled: boolean; editable: boolean }
}

export const id = (n: number): string =>
  `0192f6c4-5a1e-7000-8000-${n.toString(16).padStart(12, '0')}`

export const ROLE_ID = {
  admin: id(0x0a),
  developer: id(0x0b),
  mavenPublishers: id(0x0c),
  anonymous: id(0x0d),
  npmReaders: id(0x0e),
  dockerOperators: id(0x0f),
  auditors: id(0x10),
  ciBots: id(0x11),
  releaseManagers: id(0x12),
} as const

const PRIV = (n: number): string => id(0x200 + n)
export const PRIVILEGE_ID = {
  all: PRIV(1),
  viewAll: PRIV(2),
  anonymousRead: PRIV(3),
  mavenReleasesWrite: PRIV(4),
  mavenSnapshotsWrite: PRIV(5),
  npmRead: PRIV(6),
  npmWrite: PRIV(7),
  dockerPush: PRIV(8),
  dockerDelete: PRIV(9),
  pypiRead: PRIV(10),
  helmRead: PRIV(11),
  internalLibsRead: PRIV(12),
  noSnapshotsRead: PRIV(13),
  rawAll: PRIV(14),
} as const

export const SELECTOR_ID = {
  internalLibs: id(0x300),
  noSnapshots: id(0x301),
  dockerProd: id(0x302),
} as const

const iso = (ms: number): string => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')
const DAY = 86_400_000

/** A tiny seeded generator, so the mock shows the same data on every run. */
function sequence(seed: number): () => number {
  let state = seed
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296
    return state / 4294967296
  }
}

function seedRoles(): RoleRecord[] {
  const role = (
    key: keyof typeof ROLE_ID,
    name: string,
    description: string,
    builtin: boolean,
    privilege_ids: string[],
    nested_role_ids: string[] = [],
  ): RoleRecord => ({
    id: ROLE_ID[key],
    name,
    description,
    builtin,
    privilege_ids,
    nested_role_ids,
  })
  return [
    role('admin', 'nx-admin', 'Every action on everything, including administration', true, [
      PRIVILEGE_ID.all,
    ]),
    role('developer', 'nx-developer', 'Browse, download and publish on every repository', true, [
      PRIVILEGE_ID.viewAll,
    ]),
    role('anonymous', 'nx-anonymous', 'What a visitor without an account may do', true, [
      PRIVILEGE_ID.anonymousRead,
    ]),
    role(
      'mavenPublishers',
      'maven-publishers',
      'Publish Maven artifacts',
      false,
      [PRIVILEGE_ID.mavenReleasesWrite, PRIVILEGE_ID.mavenSnapshotsWrite],
      [ROLE_ID.developer],
    ),
    role('npmReaders', 'npm-readers', 'Read npm packages', false, [PRIVILEGE_ID.npmRead]),
    role('dockerOperators', 'docker-operators', 'Push and delete container images', false, [
      PRIVILEGE_ID.dockerPush,
      PRIVILEGE_ID.dockerDelete,
    ]),
    role('auditors', 'auditors', 'Read the internal libraries and the audit trail', false, [
      PRIVILEGE_ID.internalLibsRead,
      PRIVILEGE_ID.noSnapshotsRead,
    ]),
    role(
      'ciBots',
      'ci-bots',
      'Automation accounts of the build pipeline',
      false,
      [PRIVILEGE_ID.mavenReleasesWrite, PRIVILEGE_ID.npmWrite],
      [ROLE_ID.npmReaders],
    ),
    role(
      'releaseManagers',
      'release-managers',
      'Release owners: publish and operate images',
      false,
      [PRIVILEGE_ID.rawAll],
      [ROLE_ID.mavenPublishers, ROLE_ID.dockerOperators],
    ),
  ]
}

function seedPrivileges(): PrivilegeRecord[] {
  const view = (
    key: keyof typeof PRIVILEGE_ID,
    name: string,
    description: string,
    builtin: boolean,
    repository: string,
    format: S['RepoFormat'] | null,
    actions: PrivilegeRecord['actions'],
  ): PrivilegeRecord => ({
    id: PRIVILEGE_ID[key],
    name,
    description,
    builtin,
    grant: { type: 'repository-view', repository, format },
    actions,
  })
  const selector = (
    key: keyof typeof PRIVILEGE_ID,
    name: string,
    description: string,
    repository: string,
    selector_id: string,
    actions: PrivilegeRecord['actions'],
  ): PrivilegeRecord => ({
    id: PRIVILEGE_ID[key],
    name,
    description,
    builtin: false,
    grant: { type: 'repository-content-selector', repository, selector_id },
    actions,
  })
  return [
    {
      id: PRIVILEGE_ID.all,
      name: 'nx-all',
      description: 'Every action on everything, including administration',
      builtin: true,
      grant: { type: 'admin' },
      actions: ['browse', 'read', 'write', 'delete'],
    },
    view('viewAll', 'nx-repository-view-*-*', 'Every repository, every format', true, '*', null, [
      'browse',
      'read',
      'write',
    ]),
    view(
      'anonymousRead',
      'nx-anonymous-read',
      'Browse and download as a visitor',
      true,
      '*',
      null,
      ['browse', 'read'],
    ),
    view(
      'mavenReleasesWrite',
      'maven-releases-write',
      'Publish to maven-releases',
      false,
      'maven-releases',
      'maven2',
      ['browse', 'read', 'write'],
    ),
    view(
      'mavenSnapshotsWrite',
      'maven-snapshots-write',
      'Publish to maven-snapshots',
      false,
      'maven-snapshots',
      'maven2',
      ['browse', 'read', 'write'],
    ),
    view('npmRead', 'npm-read', 'Read every npm repository', false, '*', 'npm', ['browse', 'read']),
    view('npmWrite', 'npm-write', 'Publish to npm-releases', false, 'npm-releases', 'npm', [
      'browse',
      'read',
      'write',
    ]),
    view(
      'dockerPush',
      'docker-push',
      'Push images to docker-releases',
      false,
      'docker-releases',
      'docker',
      ['browse', 'read', 'write'],
    ),
    view(
      'dockerDelete',
      'docker-delete',
      'Delete images from docker-releases',
      false,
      'docker-releases',
      'docker',
      ['delete'],
    ),
    view('pypiRead', 'pypi-read', 'Read every PyPI repository', false, '*', 'pypi', [
      'browse',
      'read',
    ]),
    view('helmRead', 'helm-read', 'Read every Helm repository', false, '*', 'helm', [
      'browse',
      'read',
    ]),
    selector(
      'internalLibsRead',
      'internal-libs-read',
      'Read the internal libraries only',
      'maven-releases',
      SELECTOR_ID.internalLibs,
      ['browse', 'read'],
    ),
    selector(
      'noSnapshotsRead',
      'no-snapshots-read',
      'Read everything except snapshots',
      '*',
      SELECTOR_ID.noSnapshots,
      ['browse', 'read'],
    ),
    view('rawAll', 'raw-all', 'Full control of raw-releases', false, 'raw-releases', 'raw', [
      'browse',
      'read',
      'write',
      'delete',
    ]),
  ]
}

function seedSelectors(now: number): SelectorRecord[] {
  return [
    {
      id: SELECTOR_ID.internalLibs,
      name: 'internal-libs',
      description: 'Everything under com/example',
      expression: 'format == "maven2" && path.startsWith("/com/example/")',
      updated_at: iso(now - 12 * DAY),
    },
    {
      id: SELECTOR_ID.noSnapshots,
      name: 'no-snapshots',
      description: 'Release artifacts only',
      expression: '!(path =~ ".*-SNAPSHOT.*")',
      updated_at: iso(now - 40 * DAY),
    },
    {
      id: SELECTOR_ID.dockerProd,
      name: 'docker-prod',
      description: 'Images of the prod namespace',
      expression: 'format == "docker" && coordinate.name.startsWith("prod/")',
      updated_at: iso(now - 3 * DAY),
    },
  ]
}

const ISSUER = 'https://sso.example.org/realms/rustryx'

function seedUsers(now: number): UserRecord[] {
  const base = (
    n: number,
    username: string,
    source: UserRecord['source'],
    display: string,
    extra: Partial<UserRecord> = {},
  ): UserRecord => ({
    id: id(n),
    username,
    source,
    issuer: source === 'oidc' ? ISSUER : null,
    subject:
      source === 'oidc' ? `9f1c2f0e-1d2b-4b53-8f55-${n.toString(16).padStart(12, '0')}` : null,
    email: `${username}@example.org`,
    display_name: display,
    enabled: true,
    created_at: iso(now - (20 + n) * DAY),
    last_login_at: iso(now - n * 3_600_000),
    manualRoleIds: [],
    external: [],
    groupsSeen: source === 'local' ? null : [],
    tokensValidAfter: null,
    password: source === 'local' ? 'mock-password' : null,
    ...extra,
  })
  const named: UserRecord[] = [
    base(0x01, 'admin', 'local', 'Administrator', { manualRoleIds: [ROLE_ID.admin] }),
    base(0x05, 'alice', 'oidc', 'Alice Example', {
      external: [
        {
          roleId: ROLE_ID.developer,
          origin: 'oidc',
          groups: ['/rustryx/team-payments/publishers'],
        },
        {
          roleId: ROLE_ID.mavenPublishers,
          origin: 'oidc',
          groups: ['/rustryx/team-payments/publishers', '/rustryx/team-payments'],
        },
      ],
      manualRoleIds: [ROLE_ID.auditors],
      groupsSeen: ['/rustryx/team-payments/publishers', '/rustryx/team-payments'],
    }),
    base(0x06, 'bob', 'local', 'Bob Builder', {
      manualRoleIds: [ROLE_ID.mavenPublishers, ROLE_ID.npmReaders],
    }),
    base(0x07, 'carol', 'ldap', 'Carol Danvers', {
      external: [
        {
          roleId: ROLE_ID.dockerOperators,
          origin: 'ldap',
          groups: ['cn=platform,ou=groups,dc=example,dc=org'],
        },
      ],
      groupsSeen: [
        'cn=platform,ou=groups,dc=example,dc=org',
        'cn=staff,ou=groups,dc=example,dc=org',
      ],
    }),
    base(0x08, 'dave', 'oidc', 'Dave Null', {
      external: [
        { roleId: ROLE_ID.npmReaders, origin: 'oidc', groups: ['/rustryx/frontend/readers'] },
      ],
      groupsSeen: ['/rustryx/frontend/readers'],
    }),
    base(0x09, 'erin', 'local', 'Erin Disabled', { enabled: false, last_login_at: null }),
    base(0x13, 'svc-ci', 'local', 'CI service account', { manualRoleIds: [ROLE_ID.ciBots] }),
    base(0x14, 'svc-deploy', 'local', 'Deploy robot', { manualRoleIds: [ROLE_ID.dockerOperators] }),
  ]
  const next = sequence(7)
  const generated = Array.from({ length: 62 }, (_, i): UserRecord => {
    const source: UserRecord['source'] = i % 3 === 0 ? 'local' : i % 3 === 1 ? 'oidc' : 'ldap'
    const roleId =
      [ROLE_ID.developer, ROLE_ID.npmReaders, ROLE_ID.mavenPublishers, ROLE_ID.auditors][i % 4] ??
      ROLE_ID.developer
    const name = `user-${String(i + 1).padStart(3, '0')}`
    const group =
      source === 'oidc'
        ? `/rustryx/team-${(i % 5) + 1}`
        : `cn=team-${(i % 5) + 1},ou=groups,dc=example,dc=org`
    return base(0x400 + i, name, source, `Team member ${i + 1}`, {
      enabled: next() > 0.12,
      manualRoleIds: source === 'local' ? [roleId] : [],
      external: source === 'local' ? [] : [{ roleId, origin: source, groups: [group] }],
      groupsSeen: source === 'local' ? null : [group],
      last_login_at: next() > 0.2 ? iso(now - Math.floor(next() * 30 * DAY)) : null,
    })
  })
  return [...named, ...generated]
}

function seedMappings(): MappingRecord[] {
  const m = (
    n: number,
    provider: MappingRecord['provider'],
    external_group: string,
    role_id: string,
    priority: number,
  ): MappingRecord => ({ id: id(0x500 + n), provider, external_group, role_id, priority })
  return [
    m(1, 'oidc', '/rustryx/team-payments/publishers', ROLE_ID.mavenPublishers, 100),
    m(2, 'oidc', '/rustryx/team-payments/publishers', ROLE_ID.developer, 50),
    m(3, 'oidc', '/rustryx/frontend/readers', ROLE_ID.npmReaders, 100),
    m(4, 'oidc', '/rustryx/security', ROLE_ID.auditors, 200),
    m(5, 'ldap', 'cn=platform,ou=groups,dc=example,dc=org', ROLE_ID.dockerOperators, 100),
    m(6, 'ldap', 'cn=staff,ou=groups,dc=example,dc=org', ROLE_ID.developer, 300),
  ]
}

function seedTokens(users: readonly UserRecord[], now: number): TokenRecord[] {
  const owners = users.slice(0, 12)
  const make = (
    n: number,
    owner: UserRecord,
    state: 'active' | 'expired' | 'revoked' | 'unused',
  ) => {
    const created = now - (5 + n * 4) * DAY
    return {
      id: id(0x600 + n),
      user_id: owner.id,
      name:
        ['ci-publisher', 'laptop', 'deploy-bot', 'nightly-sync', 'docs-build'][n % 5] ?? 'token',
      display_prefix: `ryx_${['Qk3T', 'M9aZ', 'x2Lp', 'Vb7N', 'tR4c', 'Hd8w'][n % 6]}`,
      scopes:
        n % 3 === 0
          ? []
          : n % 3 === 1
            ? ['repo:maven-releases:write']
            : ['repo:*:read', 'repo:*:browse'],
      expires_at: iso(state === 'expired' ? now - 2 * DAY : created + 90 * DAY),
      revoked_at: state === 'revoked' ? iso(now - DAY) : null,
      last_used_at: state === 'unused' ? null : iso(now - n * 3_600_000 * 5),
      created_at: iso(created),
      kind: 'personal' as const,
    }
  }
  return Array.from({ length: 26 }, (_, n) => {
    const owner = owners[n % owners.length] ?? users[0]!
    const state = (['active', 'active', 'unused', 'expired', 'active', 'revoked'] as const)[n % 6]!
    return make(n, owner, state)
  })
}

const AUDIT_SHAPES: readonly {
  kind: AuditRecord['kind']
  action: string
  target: (n: number) => string | null
  outcome?: 'failure'
  detail: (n: number) => Record<string, unknown>
}[] = [
  {
    kind: 'login_success',
    action: 'auth.login',
    target: () => null,
    detail: () => ({ method: 'basic_local' }),
  },
  {
    kind: 'login_failure',
    action: 'auth.login',
    target: () => null,
    outcome: 'failure',
    detail: () => ({ reason: 'invalid_credentials' }),
  },
  {
    kind: 'rbac_change',
    action: 'role.update',
    target: (n) => ['maven-publishers', 'ci-bots', 'auditors'][n % 3] ?? null,
    detail: (n) => ({
      added_privileges: [['maven-releases-write', 'npm-write', 'npm-read'][n % 3]],
    }),
  },
  {
    kind: 'rbac_change',
    action: 'user.roles_set',
    target: (n) => ['bob', 'svc-ci', 'user-004'][n % 3] ?? null,
    detail: (n) => ({ role_ids: [id(0x0c + (n % 3))] }),
  },
  {
    kind: 'token_change',
    action: 'token.create',
    target: (n) => ['ci-publisher', 'laptop'][n % 2] ?? null,
    detail: () => ({ expires_in_days: 90 }),
  },
  {
    kind: 'token_change',
    action: 'token.revoke',
    target: () => 'nightly-sync',
    detail: () => ({}),
  },
  {
    kind: 'config_change',
    action: 'repository.create',
    target: (n) => `team-${String(n % 90).padStart(3, '0')}-npm`,
    detail: (n) => ({
      format: 'npm',
      repo_type: 'hosted',
      project: n % 4 === 0 ? 'payments' : null,
    }),
  },
  {
    kind: 'config_change',
    action: 'anonymous_access.repositories',
    target: () => null,
    detail: () => ({ updated: 2 }),
  },
  {
    kind: 'content_change',
    action: 'component.upload',
    target: (n) => `maven-releases/com/example/lib/1.${n % 9}`,
    detail: (n) => ({
      size_bytes: 1_048_576 + n * 4_096,
      project: n % 5 === 0 ? 'payments' : null,
    }),
  },
  {
    kind: 'content_change',
    action: 'component.delete',
    target: (n) => `npm-releases/left-pad/1.${n % 4}.0`,
    detail: () => ({}),
  },
]

function seedAudit(users: readonly UserRecord[], now: number): AuditRecord[] {
  const next = sequence(99)
  const events = Array.from({ length: 240 }, (_, n): AuditRecord => {
    const shape = AUDIT_SHAPES[Math.floor(next() * AUDIT_SHAPES.length)] ?? AUDIT_SHAPES[0]!
    const actor = users[Math.floor(next() * 9)] ?? users[0]!
    const failure = shape.outcome === 'failure'
    // Spread over 80 days with more weight on recent days: newest first, as the API sorts.
    const age = Math.floor(next() ** 2 * 80 * DAY) + n * 60_000
    return {
      id: id(0x1000 + n),
      at: iso(now - age),
      kind: shape.kind,
      actor_id: failure ? null : actor.id,
      actor_name: failure ? (['bob', 'root', 'admin', 'test'][n % 4] ?? 'bob') : actor.username,
      client_ip: `10.1.${n % 4}.${10 + (n % 200)}`,
      request_id: `0192f6c4-5a1e-7000-8000-${(0xc0de0000 + n).toString(16).padStart(12, '0')}`,
      action: shape.action,
      target: shape.target(n),
      outcome: failure ? 'failure' : 'success',
      detail: shape.detail(n),
    }
  })
  return events.sort((a, b) => b.at.localeCompare(a.at))
}

/** Fresh copy of the whole identity data set; the mock store re-seeds from it in tests. */
export function buildIdentityState(now: number = Date.now()): IdentityState {
  const users = seedUsers(now)
  return {
    roles: seedRoles(),
    privileges: seedPrivileges(),
    selectors: seedSelectors(now),
    users,
    mappings: seedMappings(),
    tokens: seedTokens(users, now),
    audit: seedAudit(users, now),
    anonymous: { globalEnabled: true, editable: false },
  }
}
