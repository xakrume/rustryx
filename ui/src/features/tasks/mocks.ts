import { http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { gate, minutesFromNow, mockId, pageOf, secondsAgo } from '@/features/system/mock-kit'
import { problem } from '@/mocks/problem'
import { json } from '@/mocks/store'

type Task = components['schemas']['Task']
type TaskRun = components['schemas']['TaskRun']
type TaskLogPage = components['schemas']['TaskLogPage']

const API = '/api/v1'

interface TaskSeed {
  n: number
  name: string
  kind: string
  description: string
  enabled: boolean
  schedule: Task['schedule']
  /** Status of the latest run; null for a task that never ran. */
  latest: TaskRun['status'] | null
  latestError: string | null
  /** Seconds the latest run took. */
  seconds: number
  agoMinutes: number
  nextInMinutes: number | null
  lockedBy: string | null
  parameters: Record<string, unknown>
}

const SEEDS: readonly TaskSeed[] = [
  {
    n: 1,
    name: 'Blob garbage collection',
    kind: 'blob-gc',
    description: 'Removes blobs that no asset refers to any more.',
    enabled: true,
    schedule: { type: 'daily', cron: null, at: null },
    latest: 'succeeded',
    latestError: null,
    seconds: 240,
    agoMinutes: 300,
    nextInMinutes: 1140,
    lockedBy: 'rustryx-0',
    parameters: { delete_untagged: true, workers: 2, minimum_age_hours: 24 },
  },
  {
    n: 2,
    name: 'Cleanup policies',
    kind: 'cleanup',
    description: 'Runs every cleanup policy against the repositories it is attached to.',
    enabled: true,
    schedule: { type: 'cron', cron: '0 2 * * *', at: null },
    latest: 'failed',
    latestError: 'blob_store_unavailable',
    seconds: 31,
    agoMinutes: 180,
    nextInMinutes: 1260,
    lockedBy: null,
    parameters: { max_items_per_run: 100000 },
  },
  {
    n: 3,
    name: 'Audit partition maintenance',
    kind: 'audit-maintenance',
    description: 'Creates next month audit partitions and drops those past retention.',
    enabled: true,
    schedule: { type: 'monthly', cron: null, at: null },
    latest: 'aborted',
    latestError: null,
    seconds: 12,
    agoMinutes: 60 * 24 * 3,
    nextInMinutes: 60 * 24 * 27,
    lockedBy: null,
    parameters: { retention_days: 90 },
  },
  {
    n: 4,
    name: 'Expired token purge',
    kind: 'token-purge',
    description: 'Deletes API tokens that expired more than a day ago.',
    enabled: true,
    schedule: { type: 'hourly', cron: null, at: null },
    latest: 'succeeded',
    latestError: null,
    seconds: 1,
    agoMinutes: 22,
    nextInMinutes: 38,
    lockedBy: null,
    parameters: {},
  },
  {
    n: 5,
    name: 'Download counter flush',
    kind: 'download-counter-flush',
    description: 'Writes download counters kept in memory to the database.',
    enabled: true,
    schedule: { type: 'cron', cron: '*/5 * * * *', at: null },
    latest: 'running',
    latestError: null,
    seconds: 0,
    agoMinutes: 0,
    nextInMinutes: 5,
    lockedBy: null,
    parameters: { batch_size: 5000 },
  },
  {
    n: 6,
    name: 'Login attempts purge',
    kind: 'login-attempts-purge',
    description: 'Forgets failed sign in counters that no longer lock anything.',
    enabled: false,
    schedule: { type: 'daily', cron: null, at: null },
    latest: null,
    latestError: null,
    seconds: 0,
    agoMinutes: 0,
    nextInMinutes: null,
    lockedBy: null,
    parameters: {},
  },
]

const NODES = ['rustryx-0', 'rustryx-1']

function summaryFor(kind: string, status: TaskRun['status']): Record<string, unknown> {
  if (status !== 'succeeded') return {}
  switch (kind) {
    case 'blob-gc':
      return { blobs_removed: 1204, bytes_reclaimed: 8_913_000_000 }
    case 'cleanup':
      return { policies: 4, components_removed: 96 }
    case 'token-purge':
      return { purged: 14 }
    case 'download-counter-flush':
      return { counters: 312 }
    default:
      return { partitions_created: 1, partitions_dropped: 0 }
  }
}

/** Run `i` of a task, newest first: mostly succeeded, now and then failed, one manual. */
function runOf(seed: TaskSeed, i: number): TaskRun {
  const latest = i === 0 && seed.latest !== null
  const status: TaskRun['status'] = latest
    ? (seed.latest as TaskRun['status'])
    : i % 11 === 7
      ? 'failed'
      : 'succeeded'
  const minutes = seed.agoMinutes + i * 1440
  const seconds = latest ? seed.seconds : 20 + ((i * 37) % 200)
  const active = status === 'running' || status === 'queued'
  const started = new Date(Date.now() - minutes * 60_000)
  const finished = new Date(started.getTime() + seconds * 1000)
  const iso = (d: Date) => d.toISOString().replace(/\.\d{3}Z$/, 'Z')
  return {
    id: mockId(8, seed.n * 100 + i),
    task_id: mockId(7, seed.n),
    status,
    dry_run: false,
    trigger: i % 9 === 4 ? 'manual' : 'schedule',
    triggered_by:
      i % 9 === 4
        ? { id: mockId(0, 1), username: 'admin', display_name: 'Administrator', source: 'local' }
        : null,
    node: NODES[i % NODES.length] ?? null,
    queued_at: iso(started),
    started_at: iso(started),
    finished_at: active ? null : iso(finished),
    duration_ms: active ? null : seconds * 1000,
    summary: summaryFor(seed.kind, status),
    error_code: status === 'failed' ? (latest ? seed.latestError : 'blob_store_unavailable') : null,
    // A purged log still answers (with `purged: true`), so the page can say when it was removed.
    log_available: true,
    log_expires_at: iso(new Date(started.getTime() + 30 * 86_400_000)),
  }
}

const RUNS_PER_TASK = 40

/** Singleton kinds take a distributed lock; the others can run on every node. */
const SINGLETONS = new Set(['blob-gc', 'cleanup', 'audit-maintenance'])

function taskOf(seed: TaskSeed): Task {
  const last = seed.latest === null ? null : runOf(seed, 0)
  return {
    id: mockId(7, seed.n),
    name: seed.name,
    kind: seed.kind,
    description: seed.description,
    system: true,
    enabled: seed.enabled,
    schedule:
      seed.schedule === null
        ? null
        : {
            ...seed.schedule,
            next_runs:
              seed.nextInMinutes === null
                ? []
                : [0, 1, 2, 3, 4].map((k) => minutesFromNow(seed.nextInMinutes! + k * 1440)),
          },
    parameters: seed.parameters,
    last_run: last,
    next_run_at: seed.nextInMinutes === null ? null : minutesFromNow(seed.nextInMinutes),
    lock: SINGLETONS.has(seed.kind) ? { held_by: seed.lockedBy } : null,
  }
}

/** Active runs move one step per read: queued, running, then succeeded. */
const reads = new Map<string, number>()

export function resetTasksMock(): void {
  reads.clear()
}

function currentRun(seed: TaskSeed, i: number): TaskRun {
  const run = runOf(seed, i)
  if (run.status !== 'running') return run
  const count = reads.get(run.id) ?? 0
  return count >= 3
    ? {
        ...run,
        status: 'succeeded',
        finished_at: secondsAgo(0),
        duration_ms: 18_000,
        summary: summaryFor(seed.kind, 'succeeded'),
      }
    : run
}

const seedOf = (taskId: string) => SEEDS.find((s) => mockId(7, s.n) === taskId)

const LINE_TEMPLATES: readonly { level: TaskLogPage['lines'][number]['level']; text: string }[] = [
  { level: 'info', text: 'Scanning blob store default' },
  { level: 'info', text: 'Read 5000 rows from the candidate table' },
  { level: 'debug', text: 'Batch 12 took 41 ms' },
  { level: 'info', text: 'Checked 25000 blobs, 312 are unreferenced' },
  { level: 'warn', text: 'Skipped a blob that a pending upload still refers to' },
  { level: 'info', text: 'Removed 300 blobs, 1.1 GiB' },
  { level: 'debug', text: 'Releasing the lock of the singleton task' },
]

function logFor(seed: TaskSeed, run: TaskRun, readCount: number): TaskLogPage['lines'] {
  const active = run.status === 'running'
  const total = active ? 14 + readCount * 9 : 40 + (run.id.length % 7) * 30
  const base = Date.parse(run.started_at ?? run.queued_at)
  return Array.from({ length: total }, (_, n) => {
    const iso = new Date(base + n * 700).toISOString().replace(/\.\d{3}Z$/, 'Z')
    if (n === 0) return { n, at: iso, level: 'info' as const, message: `Starting ${seed.kind}` }
    if (n === total - 1 && run.status === 'failed') {
      return {
        n,
        at: iso,
        level: 'error' as const,
        message: `Run failed: ${run.error_code ?? 'internal_error'}`,
      }
    }
    if (n === total - 1 && run.status === 'succeeded') {
      return { n, at: iso, level: 'info' as const, message: 'Finished without errors' }
    }
    const template = LINE_TEMPLATES[n % LINE_TEMPLATES.length] ?? LINE_TEMPLATES[0]!
    return { n, at: iso, level: template.level, message: `${template.text} (${n})` }
  })
}

const notFound = () => problem(404, 'task_not_found', 'Task not found')

export const handlers: HttpHandler[] = [
  http.get(`${API}/tasks`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const kind = url.searchParams.get('kind')
    const system = url.searchParams.get('system')
    const enabled = url.searchParams.get('enabled')
    const rows = SEEDS.map(taskOf).filter(
      (t) =>
        (kind === null || t.kind === kind) &&
        (system === null || String(t.system) === system) &&
        (enabled === null || String(t.enabled) === enabled),
    )
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.get(`${API}/tasks/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const seed = seedOf(String(params.id))
    return seed === undefined ? notFound() : json(taskOf(seed))
  }),

  http.get(`${API}/tasks/:id/runs`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const seed = seedOf(String(params.id))
    if (seed === undefined) return notFound()
    const status = new URL(request.url).searchParams.get('status')
    const rows = Array.from({ length: seed.latest === null ? 0 : RUNS_PER_TASK }, (_, i) =>
      currentRun(seed, i),
    ).filter((r) => status === null || r.status === status)
    const page = pageOf(rows, new URL(request.url), 200)
    return page instanceof Response ? page : json(page)
  }),

  http.get(`${API}/tasks/:id/runs/:run`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const seed = seedOf(String(params.id))
    if (seed === undefined) return notFound()
    const index = Array.from({ length: RUNS_PER_TASK }, (_, i) => i).find(
      (i) => runOf(seed, i).id === params.run,
    )
    if (index === undefined || seed.latest === null) {
      return problem(404, 'task_run_not_found', 'Task run not found')
    }
    const run = runOf(seed, index)
    if (run.status === 'running') reads.set(run.id, (reads.get(run.id) ?? 0) + 1)
    return json(currentRun(seed, index))
  }),

  http.get(`${API}/tasks/:id/runs/:run/log`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const seed = seedOf(String(params.id))
    if (seed === undefined) return notFound()
    const index = Array.from({ length: RUNS_PER_TASK }, (_, i) => i).find(
      (i) => runOf(seed, i).id === params.run,
    )
    if (index === undefined) return problem(404, 'task_run_not_found', 'Task run not found')
    const run = currentRun(seed, index)
    const url = new URL(request.url)
    const from = Number(url.searchParams.get('from_line') ?? 0)
    const limit = Math.min(Number(url.searchParams.get('limit') ?? 500), 2000)
    if (run.log_expires_at !== null && Date.parse(run.log_expires_at) < Date.now()) {
      const purged: TaskLogPage = {
        run_id: run.id,
        lines: [],
        next_line: null,
        finished: true,
        purged: true,
      }
      return json(purged)
    }
    const active = run.status === 'running'
    const readCount = reads.get(run.id) ?? 0
    const all = logFor(seed, run, readCount)
    const lines = all.slice(from, from + limit)
    const end = from + lines.length
    const page: TaskLogPage = {
      run_id: run.id,
      lines,
      next_line: end < all.length || active ? end : null,
      finished: !active,
      purged: false,
    }
    return json(page)
  }),
]
