import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { daysAgo, gate, hoursAgo, mockId, pageOf, secondsAgo } from '@/features/system/mock-kit'
import { problem } from '@/mocks/problem'
import { json } from '@/mocks/store'

type LifecycleRun = components['schemas']['LifecycleRun']
type LifecycleRunSummary = components['schemas']['LifecycleRunSummary']
type LifecycleCandidate = components['schemas']['LifecycleCandidate']
type GcRun = components['schemas']['GcRun']
type Criteria = components['schemas']['CleanupCriteria']

const API = '/api/v1'
const MIB = 1024 ** 2

/** Fixed ids so the cleanup policy fixtures and the run history agree. */
export const POLICY_IDS = {
  maven: mockId(2, 1),
  npm: mockId(2, 2),
  docker: mockId(2, 3),
  pypi: mockId(2, 4),
  raw: mockId(2, 5),
  helm: mockId(2, 6),
} as const

interface RunRecord {
  run: LifecycleRun
  repositories: readonly string[]
  reason: LifecycleCandidate['reason']
  /** Reads of an active run, so a poll advances it: queued, running, succeeded. */
  reads: number
}

const ADMIN = {
  id: mockId(0, 1),
  username: 'admin',
  display_name: 'Administrator',
  source: 'local' as const,
}

function seedRun(
  n: number,
  policy: { id: string; name: string },
  over: Partial<LifecycleRun>,
  repositories: readonly string[],
  reason: LifecycleCandidate['reason'],
): RunRecord {
  return {
    run: {
      id: mockId(3, n),
      kind: 'cleanup',
      policy_id: policy.id,
      policy_name: policy.name,
      dry_run: true,
      status: 'succeeded',
      trigger: 'manual',
      triggered_by: ADMIN,
      task_run_id: mockId(4, n),
      queued_at: hoursAgo(1),
      started_at: hoursAgo(1),
      finished_at: hoursAgo(1),
      summary: null,
      error_code: null,
      report_expires_at: daysAgo(-30),
      ...over,
    },
    repositories,
    reason,
    reads: 0,
  }
}

const POLICIES = {
  maven: { id: POLICY_IDS.maven, name: 'maven-old-snapshots' },
  npm: { id: POLICY_IDS.npm, name: 'npm-stale-packages' },
  docker: { id: POLICY_IDS.docker, name: 'docker-tag-cleanup' },
  pypi: { id: POLICY_IDS.pypi, name: 'pypi-old-dev-builds' },
  raw: { id: POLICY_IDS.raw, name: 'raw-temp-files' },
  helm: { id: POLICY_IDS.helm, name: 'helm-old-charts' },
}

const iso = (ms: number): string => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')

function seedRuns(): RunRecord[] {
  return [
    seedRun(
      1,
      POLICIES.maven,
      {
        queued_at: secondsAgo(12 * 60),
        started_at: secondsAgo(12 * 60 - 2),
        finished_at: secondsAgo(11 * 60),
        summary: { candidates: 412, bytes: 19_541_000_000, repositories: 2, protected: 6 },
      },
      ['maven-snapshots', 'maven-releases'],
      'older_than_days',
    ),
    seedRun(
      2,
      POLICIES.npm,
      {
        dry_run: false,
        trigger: 'schedule',
        triggered_by: null,
        queued_at: daysAgo(1),
        started_at: daysAgo(1),
        finished_at: iso(Date.now() - 86_400_000 + 95_000),
        summary: { candidates: 96, bytes: 2_254_857_830, repositories: 1, protected: 0 },
      },
      ['npm-snapshots'],
      'last_downloaded_days',
    ),
    seedRun(
      3,
      POLICIES.maven,
      {
        dry_run: false,
        trigger: 'schedule',
        triggered_by: null,
        status: 'failed',
        queued_at: daysAgo(2),
        started_at: daysAgo(2),
        finished_at: iso(Date.now() - 2 * 86_400_000 + 31_000),
        error_code: 'blob_store_unavailable',
      },
      ['maven-snapshots'],
      'older_than_days',
    ),
    seedRun(
      4,
      POLICIES.docker,
      {
        status: 'running',
        queued_at: secondsAgo(20),
        started_at: secondsAgo(18),
        finished_at: null,
      },
      ['docker-snapshots'],
      'name_pattern',
    ),
    seedRun(
      5,
      POLICIES.pypi,
      {
        queued_at: daysAgo(40),
        started_at: daysAgo(40),
        finished_at: daysAgo(40),
        report_expires_at: daysAgo(10),
        summary: { candidates: 58, bytes: 310_000_000, repositories: 1, protected: 0 },
      },
      ['pypi-snapshots'],
      'name_pattern',
    ),
    seedRun(
      6,
      POLICIES.raw,
      {
        status: 'cancelled',
        queued_at: daysAgo(4),
        started_at: daysAgo(4),
        finished_at: daysAgo(4),
      },
      ['raw-snapshots'],
      'older_than_days',
    ),
    seedRun(
      7,
      POLICIES.helm,
      {
        dry_run: false,
        trigger: 'api',
        queued_at: daysAgo(3),
        started_at: daysAgo(3),
        finished_at: daysAgo(3),
        summary: { candidates: 0, bytes: 0, repositories: 0, protected: 0 },
      },
      [],
      'older_than_days',
    ),
  ]
}

function seedGc(): GcRun[] {
  const run = (n: number, over: Partial<GcRun>): GcRun => ({
    id: mockId(5, n),
    trigger: 'schedule',
    dry_run: false,
    delete_untagged: true,
    workers: 2,
    status: 'succeeded',
    queued_at: daysAgo(n),
    started_at: daysAgo(n),
    finished_at: iso(Date.now() - n * 86_400_000 + 240_000),
    candidate_blobs: 1204,
    reclaimed_bytes: 8_913_000_000,
    per_blob_store: [
      { blob_store: 'default', candidates: 1100, bytes: 8_200_000_000 },
      { blob_store: 'fast-ssd', candidates: 104, bytes: 713_000_000 },
    ],
    task_run_id: mockId(4, 100 + n),
    log_available: true,
    ...over,
  })
  return [
    run(1, { dry_run: true, trigger: 'manual' }),
    run(2, {}),
    run(3, { status: 'failed', candidate_blobs: null, reclaimed_bytes: null, per_blob_store: [] }),
    run(4, {
      status: 'stopped',
      candidate_blobs: 300,
      reclaimed_bytes: 1_100_000_000,
      per_blob_store: [{ blob_store: 'default', candidates: 300, bytes: 1_100_000_000 }],
    }),
  ]
}

let runs: readonly RunRecord[] = seedRuns()
let gcRuns: readonly GcRun[] = seedGc()
let counter = 0

export function resetLifecycleMock(): void {
  runs = seedRuns()
  gcRuns = seedGc()
  counter = 0
}

/** How many items a plan holds for these criteria: a stricter policy removes less. */
function estimate(criteria: Criteria): number {
  const age = criteria.older_than_days ?? 30
  const base = Math.max(3, Math.round(900 / (1 + age / 20)))
  const halved = criteria.last_downloaded_days === null ? base : Math.round(base / 2)
  return criteria.keep_last_versions === null ? halved : halved + 40
}

function firstReason(criteria: Criteria): LifecycleCandidate['reason'] {
  if (criteria.older_than_days !== null) return 'older_than_days'
  if (criteria.last_downloaded_days !== null) return 'last_downloaded_days'
  if (criteria.keep_last_versions !== null) return 'keep_last_versions'
  if (criteria.name_pattern !== null) return 'name_pattern'
  return 'release_type'
}

export interface PreviewTarget {
  id: string
  name: string
  criteria: Criteria
  repositories: readonly string[]
}

/** What a started run will report once it succeeds, by run id. */
const planned = new Map<
  string,
  { candidates: number; bytes: number; repositories: number; protectedItems: number }
>()

/** Start a dry run of a policy: 202 with the queued run. Reads of the run advance it. */
export function startDryRun(policy: PreviewTarget): LifecycleRun {
  counter += 1
  const candidates = estimate(policy.criteria)
  const record = seedRun(
    100 + counter,
    { id: policy.id, name: policy.name },
    {
      status: 'queued',
      queued_at: secondsAgo(0),
      started_at: null,
      finished_at: null,
      // The totals are known once the run succeeds; the record keeps them for that moment.
      summary: null,
      report_expires_at: daysAgo(-30),
    },
    policy.repositories,
    firstReason(policy.criteria),
  )
  const withPlan: RunRecord = {
    ...record,
    run: { ...record.run },
    reads: 0,
  }
  planned.set(withPlan.run.id, {
    candidates,
    bytes: candidates * 31 * MIB,
    repositories: Math.max(policy.repositories.length, 1),
    protectedItems: Math.floor(candidates / 60),
  })
  runs = [withPlan, ...runs]
  return withPlan.run
}

export function lastRunSummary(policyId: string): LifecycleRunSummary | null {
  const latest = runs.find((r) => r.run.policy_id === policyId)
  if (latest === undefined) return null
  const { run } = latest
  return {
    id: run.id,
    dry_run: run.dry_run,
    status: run.status === 'queued' || run.status === 'running' ? run.status : run.status,
    finished_at: run.finished_at,
    candidates: run.summary?.candidates ?? null,
    bytes: run.summary?.bytes ?? null,
  }
}

/** The run as the server shows it now: an active run moves one step per read. */
function advance(id: string): LifecycleRun | undefined {
  const record = runs.find((r) => r.run.id === id)
  if (record === undefined) return undefined
  if (record.run.status !== 'queued' && record.run.status !== 'running') return record.run
  const reads = record.reads + 1
  const plan = planned.get(id)
  const done = reads >= 3
  const next: LifecycleRun = done
    ? {
        ...record.run,
        status: 'succeeded',
        started_at: record.run.started_at ?? secondsAgo(3),
        finished_at: secondsAgo(0),
        summary: {
          candidates: plan?.candidates ?? 128,
          bytes: plan?.bytes ?? 128 * 31 * MIB,
          repositories: plan?.repositories ?? Math.max(record.repositories.length, 1),
          protected: plan?.protectedItems ?? 2,
        },
      }
    : { ...record.run, status: 'running', started_at: record.run.started_at ?? secondsAgo(1) }
  runs = runs.map((r) => (r.run.id === id ? { ...r, run: next, reads } : r))
  return next
}

function candidateAt(record: RunRecord, index: number): LifecycleCandidate {
  const repositories = record.repositories.length > 0 ? record.repositories : ['maven-snapshots']
  const repository = repositories[index % repositories.length] ?? 'maven-snapshots'
  const protectedEvery = 60
  const isProtected = index % protectedEvery === protectedEvery - 1
  const age = 91 + ((index * 7) % 400)
  return {
    repository,
    component_id: mockId(6, index + 1),
    asset_id: null,
    coordinate: `com.example.payments:ledger-${(index % 40) + 1}:2.${index}.0-SNAPSHOT`,
    size_bytes: (((index * 2654435761) >>> 0) % (380 * MIB)) + 20_000,
    age_days: age,
    last_downloaded_at: index % 3 === 0 ? null : daysAgo(age - 30),
    reason: record.reason,
    protected_by: isProtected ? 'immutability rule release-lock' : null,
  }
}

function allCandidates(record: RunRecord): LifecycleCandidate[] {
  const total = record.run.summary?.candidates ?? 0
  return Array.from({ length: total }, (_, i) => candidateAt(record, i))
}

const csvEscape = (value: string | number | null): string => {
  const text = value === null ? '' : String(value)
  return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text
}

const notFound = () => problem(404, 'lifecycle_run_not_found', 'Lifecycle run not found')

export const handlers: HttpHandler[] = [
  http.get(`${API}/lifecycle-runs`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const dry = url.searchParams.get('dry_run')
    const status = url.searchParams.get('status')
    const policy = url.searchParams.get('policy_id')
    const rows = runs
      .map((r) => r.run)
      .filter(
        (r) =>
          (dry === null || String(r.dry_run) === dry) &&
          (status === null || r.status === status) &&
          (policy === null || r.policy_id === policy),
      )
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.get(`${API}/lifecycle-runs/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const run = advance(String(params.id))
    return run === undefined ? notFound() : json(run)
  }),

  http.get(`${API}/lifecycle-runs/:id/candidates`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = runs.find((r) => r.run.id === params.id)
    if (record === undefined) return notFound()
    const expiry = record.run.report_expires_at
    if (expiry !== null && new Date(expiry).getTime() < Date.now()) return notFound()
    if (record.run.status === 'queued' || record.run.status === 'running') {
      return problem(409, 'not_ready', 'The report is not ready yet')
    }
    const url = new URL(request.url)
    const repository = url.searchParams.get('repository')
    const onlyProtected = url.searchParams.get('protected')
    const rows = allCandidates(record).filter(
      (c) =>
        (repository === null || c.repository === repository) &&
        (onlyProtected === null || String(c.protected_by !== null) === onlyProtected),
    )
    if ((request.headers.get('accept') ?? '').includes('text/csv')) {
      const header =
        'repository,coordinate,size_bytes,age_days,last_downloaded_at,reason,protected_by'
      const lines = rows.map((c) =>
        [
          c.repository,
          c.coordinate,
          c.size_bytes,
          c.age_days,
          c.last_downloaded_at,
          c.reason,
          c.protected_by,
        ]
          .map(csvEscape)
          .join(','),
      )
      return new HttpResponse([header, ...lines].join('\r\n') + '\r\n', {
        headers: {
          'Content-Type': 'text/csv',
          'Content-Disposition': `attachment; filename="cleanup-plan-${record.run.id}.csv"`,
        },
      })
    }
    const page = pageOf(rows, url, 200)
    return page instanceof Response ? page : json(page)
  }),

  http.get(`${API}/gc/runs`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const dry = url.searchParams.get('dry_run')
    const status = url.searchParams.get('status')
    const rows = gcRuns.filter(
      (r) =>
        (dry === null || String(r.dry_run) === dry) && (status === null || r.status === status),
    )
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.get(`${API}/gc/runs/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const run = gcRuns.find((r) => r.id === params.id)
    return run === undefined ? problem(404, 'gc_run_not_found', 'GC run not found') : json(run)
  }),

  http.get(`${API}/gc/runs/:id/candidates`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const run = gcRuns.find((r) => r.id === params.id)
    if (run === undefined) return problem(404, 'gc_run_not_found', 'GC run not found')
    const total = Math.min(run.candidate_blobs ?? 0, 240)
    const rows = Array.from({ length: total }, (_, i) => ({
      blob_store:
        run.per_blob_store[i % Math.max(run.per_blob_store.length, 1)]?.blob_store ?? 'default',
      key: `ab/${(i * 7919).toString(16).padStart(8, '0')}-${(i * 104729).toString(16).padStart(8, '0')}`,
      size_bytes: 40_000 + (((i * 2654435761) >>> 0) % (90 * MIB)),
      orphaned_at: daysAgo(2 + (i % 20)),
    }))
    const page = pageOf(rows, new URL(request.url), 200)
    return page instanceof Response ? page : json(page)
  }),
]
