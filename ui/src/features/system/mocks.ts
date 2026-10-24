import { http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { gate, hoursAgo, secondsAgo } from '@/features/system/mock-kit'
import { json } from '@/mocks/store'

type StatusCheck = components['schemas']['StatusCheck']
type SystemStatus = components['schemas']['SystemStatus']

const API = '/api/v1'
const STARTED = Date.now()

/**
 * `sessionStorage['ryx-mock-status']` picks the scenario the browser tests look at:
 * `healthy` (the default: every check passes, so the shell shows no degraded banner on pages of
 * other features), `mixed` (a degraded blob store, a failed job), `dependencies-down` (Keycloak,
 * LDAP and Redis down while the node stays READY), `not-ready` (PostgreSQL fails, so the node is
 * not ready while the dependency list is green).
 */
export type StatusScenario = 'mixed' | 'healthy' | 'dependencies-down' | 'not-ready'

function scenario(): StatusScenario {
  try {
    const value = window.sessionStorage.getItem('ryx-mock-status')
    if (value === 'mixed' || value === 'dependencies-down' || value === 'not-ready') return value
  } catch {
    // Storage blocked: the default scenario.
  }
  return 'healthy'
}

function check(
  id: string,
  title: string,
  status: StatusCheck['status'],
  severity: StatusCheck['severity'],
  message: string,
  extra: Partial<StatusCheck> = {},
): StatusCheck {
  return { id, title, status, severity, message, fix_hint: null, since: null, link: null, ...extra }
}

function checks(mode: StatusScenario): StatusCheck[] {
  const mixed = mode === 'mixed'
  const depsDown = mode === 'dependencies-down'
  const dbDown = mode === 'not-ready'
  return [
    check(
      'database',
      'PostgreSQL',
      dbDown ? 'error' : 'ok',
      'critical',
      dbDown ? 'PostgreSQL did not answer within 1 second' : 'PostgreSQL answered in 3 ms',
      dbDown ? { since: secondsAgo(95) } : {},
    ),
    check(
      'redis',
      'Redis',
      depsDown ? 'error' : 'ok',
      'warning',
      depsDown ? 'Redis refused the connection' : 'Redis answered in 1 ms',
      depsDown ? { since: secondsAgo(600) } : {},
    ),
    check(
      'blob_store_health',
      'Blob store health',
      mixed ? 'warning' : 'ok',
      'critical',
      mixed ? 'archive-s3 is degraded and cold-s3 is unreachable' : 'All 3 blob stores answer',
      !mixed
        ? {}
        : {
            since: hoursAgo(3),
            link: '/admin/blob-stores',
            fix_hint: 'Open the blob store and run the connection test.',
          },
    ),
    check(
      'oidc_reachable',
      'Keycloak',
      depsDown ? 'error' : 'ok',
      'warning',
      depsDown
        ? 'Keycloak did not answer: new single sign-on logins fail'
        : 'Keycloak realm rustryx answers',
      depsDown ? { since: secondsAgo(1200) } : {},
    ),
    check(
      'ldap_reachable',
      'LDAP',
      depsDown ? 'error' : mixed ? 'unknown' : 'ok',
      'info',
      depsDown
        ? 'The directory refused the bind'
        : mixed
          ? 'The check has not run yet on this node'
          : 'The directory answers in 12 ms',
    ),
    check('schema_version', 'Database schema', 'ok', 'critical', 'Schema 42 matches the binary'),
    check(
      'default_credentials',
      'Default credentials',
      'ok',
      'critical',
      'The local administrator uses a changed password',
    ),
    check(
      'anonymous_access',
      'Anonymous access',
      mixed ? 'warning' : 'ok',
      'warning',
      mixed ? 'Anonymous read is on for 4 repositories' : 'Anonymous read is off',
      !mixed ? {} : { link: '/admin/anonymous', fix_hint: 'Review which repositories are public.' },
    ),
    check(
      'blob_store_quota',
      'Blob store quota',
      mixed ? 'warning' : 'ok',
      'warning',
      mixed
        ? 'reports is over its soft quota (104 percent)'
        : 'Every store is within its soft quota',
      !mixed
        ? {}
        : {
            link: '/admin/blob-stores/reports',
            fix_hint: 'Raise the quota or move content to another store.',
          },
    ),
    check(
      'background_jobs',
      'Background jobs',
      mixed ? 'warning' : 'ok',
      'warning',
      mixed ? 'The cleanup job failed on its last run' : 'Every job succeeded recently',
      !mixed ? {} : { link: '/admin/tasks', fix_hint: 'Open the run and read its log.' },
    ),
    check(
      'audit_partitions',
      'Audit partitions',
      'ok',
      'info',
      'Partitions exist for this and next month',
    ),
    check('read_only_mode', 'Read only mode', 'ok', 'info', 'The system accepts writes'),
  ]
}

function body(): SystemStatus {
  const mode = scenario()
  const list = checks(mode)
  const worst: SystemStatus['status'] = list.some((c) => c.status === 'error')
    ? 'error'
    : list.some((c) => c.status === 'warning')
      ? 'warning'
      : list.some((c) => c.status === 'unknown')
        ? 'unknown'
        : 'ok'
  const notReady = mode === 'not-ready'
  const dependencyProblem = mode === 'dependencies-down' || mode === 'mixed'
  return {
    status: worst,
    generated_at: secondsAgo(2),
    node: 'rustryx-0',
    checks: list,
    readiness: {
      status: notReady ? 'not_ready' : 'ready',
      checks: [
        { name: 'listening', status: 'ok' },
        { name: 'draining', status: 'ok' },
        { name: 'schema', status: 'ok' },
        { name: 'database', status: notReady ? 'fail' : 'ok' },
      ],
      advisories: [
        { name: 'redis', status: mode === 'dependencies-down' ? 'degraded' : 'ok' },
        { name: 'oidc', status: mode === 'dependencies-down' ? 'degraded' : 'ok' },
        { name: 'ldap', status: mode === 'dependencies-down' ? 'degraded' : 'ok' },
        { name: 'blob_stores', status: dependencyProblem ? 'degraded' : 'ok' },
      ],
    },
  }
}

export const handlers: HttpHandler[] = [
  http.get(`${API}/system/status`, async ({ request }) => {
    const refused = await gate(request)
    return refused ?? json(body())
  }),

  http.get(`${API}/system/nodes`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const nodes = [
      {
        id: 'rustryx-0',
        version: '0.1.0',
        started_at: new Date(STARTED - 4 * 86_400_000 - 5_400_000)
          .toISOString()
          .replace(/\.\d{3}Z$/, 'Z'),
        last_heartbeat_at: secondsAgo(4),
        draining: false,
        leader_of: ['blob-gc', 'cleanup'],
      },
      {
        id: 'rustryx-1',
        version: '0.1.0',
        started_at: new Date(STARTED - 2 * 86_400_000).toISOString().replace(/\.\d{3}Z$/, 'Z'),
        last_heartbeat_at: secondsAgo(scenario() === 'mixed' ? 300 : 6),
        draining: false,
        leader_of: ['audit-maintenance'],
      },
    ]
    return json({ items: nodes, next: null })
  }),

  http.get(`${API}/system/info/details`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    return json({
      schema_version: 42,
      node: 'rustryx-0',
      configuration: {
        server: { base_url: 'https://repo.example.org' },
        database: { url: '[REDACTED]' },
      },
    })
  }),

  http.get(`${API}/system/metrics/summary`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const elapsed = Math.floor((Date.now() - STARTED) / 1000)
    return json({
      node: 'rustryx-0',
      generated_at: secondsAgo(0),
      uptime_secs: 4 * 86_400 + 3_600 + elapsed,
      http: {
        requests_total: 1_482_113 + elapsed * 3,
        errors_5xx_total: 1_912,
        in_flight: 3,
        shed_total: 14,
        p95_ms: 87.4,
      },
      auth: { logins_ok_total: 2_310, logins_failed_total: 41, lockouts_total: 2 },
      database: { pool_size: 20, pool_in_use: 4, pool_wait_p95_ms: 1.2 },
      background_jobs: [
        { task: 'blob-gc', last_success_at: hoursAgo(5), stale: false },
        { task: 'cleanup', last_success_at: hoursAgo(52), stale: scenario() === 'mixed' },
        { task: 'token-purge', last_success_at: secondsAgo(22 * 60), stale: false },
        { task: 'download-counter-flush', last_success_at: null, stale: false },
      ],
    })
  }),
]
