import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import {
  checkIfMatch,
  daysAgo,
  etagOf,
  gate,
  hoursAgo,
  invalidInput,
  mockId,
  pageOf,
  readJsonBody,
  secondsAgo,
} from '@/features/system/mock-kit'
import { problem } from '@/mocks/problem'
import { json, repositories } from '@/mocks/store'

type BlobStoreView = components['schemas']['BlobStoreView']
type BlobStoreCreate = components['schemas']['BlobStoreCreate']
type BlobStorePatch = components['schemas']['BlobStorePatch']
type Credentials = components['schemas']['BlobStoreCredentialsState']

const GIB = 1024 ** 3
const API = '/api/v1'
const KIND = 'blob-store'

interface StoreRecord {
  view: BlobStoreView
  version: number
  /** Names of the repositories that use the store. */
  repositories: readonly string[]
  /** Daily used-bytes samples, oldest first: the optional `usage.history` the dashboard chart reads. */
  history: readonly number[]
}

function usage(used: number, blobs: number, total: number | null) {
  return {
    used_bytes: used,
    blob_count: blobs,
    available_bytes: total === null ? null : total - used,
    total_bytes: total,
    as_of: secondsAgo(40),
  }
}

function view(
  n: number,
  name: string,
  kind: 'local' | 's3',
  config: Record<string, unknown>,
  quotaGiB: number | null,
  health: BlobStoreView['health'],
  stats: BlobStoreView['usage'],
  credentials: Credentials | null,
  repositoryCount: number,
): BlobStoreView {
  return {
    id: mockId(1, n),
    name,
    kind,
    config,
    quota_bytes: quotaGiB === null ? null : quotaGiB * GIB,
    created_at: daysAgo(90 - n * 7),
    usage: stats,
    health,
    repository_count: repositoryCount,
    credentials,
  }
}

const healthy = () => ({ state: 'ok' as const, checked_at: secondsAgo(40), last_error_code: null })

function growth(end: number, days: number, perDay: number): number[] {
  return Array.from({ length: days }, (_, i) => Math.max(0, end - (days - 1 - i) * perDay))
}

function seed(): StoreRecord[] {
  const s3 = (bucket: string) => ({
    endpoint: 'https://seaweedfs.example.org',
    bucket,
    region: 'us-east-1',
    key_prefix: '',
    path_style: true,
    part_size_mb: 16,
  })
  const defaultUsed = 384 * GIB
  const ssdUsed = 91 * GIB
  const reportsUsed = 52 * GIB
  const archiveUsed = 8_123_456_789
  return [
    {
      view: view(
        1,
        'default',
        'local',
        { path: '/var/lib/rustryx/blobs' },
        1024,
        healthy(),
        usage(defaultUsed, 1_841_220, 2048 * GIB),
        null,
        repositories.length,
      ),
      version: 1,
      repositories: repositories.map((r) => r.name),
      history: growth(defaultUsed, 14, 2.5 * GIB),
    },
    {
      view: view(
        2,
        'fast-ssd',
        'local',
        { path: '/mnt/ssd/blobs' },
        100,
        healthy(),
        usage(ssdUsed, 412_330, 240 * GIB),
        null,
        2,
      ),
      version: 1,
      repositories: ['maven-releases', 'docker-releases'],
      history: growth(ssdUsed, 14, 1.8 * GIB),
    },
    {
      view: view(
        3,
        'archive-s3',
        's3',
        s3('rustryx-archive'),
        null,
        { state: 'degraded', checked_at: secondsAgo(40), last_error_code: 'upstream_timeout' },
        usage(archiveUsed, 15_022, null),
        { configured: true, source: 'inline', access_key_ref: null, secret_key_ref: null },
        3,
      ),
      version: 1,
      repositories: ['maven-snapshots', 'pypi-snapshots', 'raw-snapshots'],
      history: growth(archiveUsed, 14, 0.2 * GIB),
    },
    {
      view: view(
        4,
        'cold-s3',
        's3',
        s3('rustryx-cold'),
        null,
        {
          state: 'unreachable',
          checked_at: secondsAgo(40),
          last_error_code: 'blob_store_unavailable',
        },
        null,
        {
          configured: true,
          source: 'reference',
          access_key_ref: 'env:COLD_S3_ACCESS_KEY',
          secret_key_ref: 'env:COLD_S3_SECRET_KEY',
        },
        1,
      ),
      version: 1,
      repositories: ['helm-releases'],
      history: [],
    },
    {
      view: view(
        5,
        'reports',
        'local',
        { path: '/srv/reports/blobs' },
        50,
        healthy(),
        usage(reportsUsed, 8_204, 80 * GIB),
        null,
        1,
      ),
      version: 1,
      repositories: ['raw-releases'],
      history: growth(reportsUsed, 14, 0.9 * GIB),
    },
    {
      view: view(
        6,
        'scratch',
        'local',
        { path: '/srv/scratch/blobs' },
        null,
        healthy(),
        usage(0, 0, 80 * GIB),
        null,
        0,
      ),
      version: 1,
      repositories: [],
      history: [],
    },
  ]
}

let state: readonly StoreRecord[] = seed()

/** Tests start every case from the same fixtures. */
export function resetBlobStoreMock(): void {
  state = seed()
}

const find = (name: string) => state.find((s) => s.view.name === name)

const notFound = () => problem(404, 'blob_store_not_found', 'Blob store not found')

/** The wire form: `usage.history` is the optional trend the dashboard widget uses when present. */
function present(record: StoreRecord): BlobStoreView {
  const { view: v, history } = record
  if (v.usage === null || history.length === 0) return v
  const points = history.map((used_bytes, i) => ({
    at: hoursAgo((history.length - 1 - i) * 24),
    used_bytes,
  }))
  return { ...v, usage: { ...v.usage, history: points } as typeof v.usage }
}

function replace(next: StoreRecord): void {
  state = state.map((s) => (s.view.name === next.view.name ? next : s))
}

const PRIVATE_HOST = /^(localhost|127\.|10\.|169\.254\.|192\.168\.)/

function checkCreate(body: BlobStoreCreate): Response | null {
  if (body.kind === 's3') {
    const host = (() => {
      try {
        return new URL(body.config.endpoint).hostname
      } catch {
        return ''
      }
    })()
    if (PRIVATE_HOST.test(host)) {
      return problem(400, 'upstream_url_rejected', 'The endpoint address is not allowed', {
        details: { rule: 'private_address' },
      })
    }
    if (host.includes('unreachable')) {
      return invalidInput('/config/endpoint', 'The store did not accept the probe object')
    }
  } else if (body.config.path.startsWith('/proc') || body.config.path.startsWith('/nonexistent')) {
    return invalidInput('/config/path', 'The directory cannot be written by the server')
  }
  return null
}

function credentialsOf(body: BlobStoreCreate | BlobStorePatch): Credentials | null | undefined {
  const given = 'credentials' in body ? body.credentials : undefined
  if (given === undefined) return undefined
  const hasRefs = given.access_key_ref !== undefined || given.secret_key_ref !== undefined
  return {
    configured: true,
    source: hasRefs ? 'reference' : 'inline',
    access_key_ref: given.access_key_ref ?? null,
    secret_key_ref: given.secret_key_ref ?? null,
  }
}

const IMMUTABLE_IN_USE = ['path', 'bucket', 'endpoint'] as const

export const handlers: HttpHandler[] = [
  http.get(`${API}/blob-stores`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const q = url.searchParams.get('q')?.toLowerCase()
    const kind = url.searchParams.get('kind')
    const rows = state
      .filter(
        (s) =>
          (q === undefined || s.view.name.toLowerCase().startsWith(q)) &&
          (kind === null || s.view.kind === kind),
      )
      .map(present)
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.post(`${API}/blob-stores`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const body = await readJsonBody<BlobStoreCreate>(request)
    if (
      body === null ||
      typeof body.name !== 'string' ||
      (body.kind !== 'local' && body.kind !== 's3')
    ) {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    if (find(body.name) !== undefined) {
      return problem(409, 'already_exists', 'A blob store with this name already exists')
    }
    const rejected = checkCreate(body)
    if (rejected) return rejected
    const record: StoreRecord = {
      view: {
        id: mockId(1, 100 + state.length),
        name: body.name,
        kind: body.kind,
        config: { ...body.config },
        quota_bytes: body.quota_bytes ?? null,
        created_at: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
        usage: usage(0, 0, body.kind === 'local' ? 80 * GIB : null),
        health: healthy(),
        repository_count: 0,
        credentials:
          body.kind === 's3'
            ? (credentialsOf(body) ?? {
                configured: true,
                source: 'environment',
                access_key_ref: null,
                secret_key_ref: null,
              })
            : null,
      },
      version: 1,
      repositories: [],
      history: [],
    }
    state = [...state, record]
    return json(record.view, {
      status: 201,
      headers: {
        ETag: etagOf(KIND, record.view.name, 1),
        Location: `${API}/blob-stores/${record.view.name}`,
      },
    })
  }),

  http.get(`${API}/blob-stores/:name`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = find(String(params.name))
    if (record === undefined) return notFound()
    return json(present(record), {
      headers: { ETag: etagOf(KIND, record.view.name, record.version) },
    })
  }),

  http.patch(`${API}/blob-stores/:name`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = find(String(params.name))
    if (record === undefined) return notFound()
    const stale = checkIfMatch(request, etagOf(KIND, record.view.name, record.version))
    if (stale) return stale
    const patch = await readJsonBody<BlobStorePatch>(request)
    if (patch === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    const locked = IMMUTABLE_IN_USE.find(
      (key) =>
        record.view.repository_count > 0 &&
        patch.config?.[key] !== undefined &&
        patch.config[key] !== record.view.config[key],
    )
    if (locked !== undefined) {
      return problem(
        400,
        'immutable_field',
        'This setting cannot change while repositories use the store',
        {
          details: {
            fields: [
              { field: `/config/${locked}`, code: 'immutable', message: 'In use by repositories' },
            ],
          },
        },
      )
    }
    const next: StoreRecord = {
      ...record,
      version: record.version + 1,
      view: {
        ...record.view,
        config: { ...record.view.config, ...patch.config },
        quota_bytes: patch.quota_bytes === undefined ? record.view.quota_bytes : patch.quota_bytes,
        credentials: credentialsOf(patch) ?? record.view.credentials,
      },
    }
    replace(next)
    return json(present(next), { headers: { ETag: etagOf(KIND, next.view.name, next.version) } })
  }),

  http.delete(`${API}/blob-stores/:name`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = find(String(params.name))
    if (record === undefined) return notFound()
    if (record.view.repository_count > 0) {
      return problem(409, 'resource_in_use', 'Repositories still use this blob store', {
        details: {
          dependents: record.repositories
            .slice(0, 10)
            .map((name) => ({ kind: 'repository', id: null, name })),
        },
      })
    }
    state = state.filter((s) => s.view.name !== record.view.name)
    return new HttpResponse(null, { status: 204 })
  }),

  http.get(`${API}/blob-stores/:name/usage`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = find(String(params.name))
    if (record === undefined) return notFound()
    if (record.view.usage === null) {
      return problem(503, 'blob_store_unavailable', 'The blob store is unreachable', {
        retryAfterSecs: 5,
      })
    }
    return json({ ...record.view.usage, as_of: secondsAgo(1) })
  }),

  http.post(`${API}/blob-stores/:name/actions/test-connection`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const record = find(String(params.name))
    if (record === undefined) return notFound()
    const { state: health, last_error_code: code } = record.view.health
    if (health === 'unreachable') {
      return json({ ok: false, latency_ms: null, error_code: code ?? 'blob_store_unavailable' })
    }
    return json({ ok: true, latency_ms: health === 'degraded' ? 840 : 12, error_code: null })
  }),

  // The shared repository list ignores `blob_store`; answer it for the stores of this feature
  // and fall through (no response) for every other request so the real handler keeps its job.
  http.get(`${API}/repositories`, async ({ request }) => {
    const url = new URL(request.url)
    const store = url.searchParams.get('blob_store')
    const record = store === null ? undefined : find(store)
    if (record === undefined || record.view.name === 'default') return undefined
    const refused = await gate(request)
    if (refused) return refused
    const rows = repositories.filter((r) => record.repositories.includes(r.name))
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),
]
