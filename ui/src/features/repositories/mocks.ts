import { delay, http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { ownComponents } from '@/features/components/mock-content'
import { requireSession } from '@/mocks/auth-guard'
import { problem } from '@/mocks/problem'
import { json, LATENCY_MS, repositories, visibleRepositories } from '@/mocks/store'
import { MOCK_FORMATS, mockUploadSchema } from './mock-formats'
import { paginate } from './mock-page'
import {
  bumpVersion,
  blobStoreRef,
  CLEANUP_POLICY_NAMES,
  etagOf,
  findVisible,
  present,
  replaceRepository,
  ROUTING_RULE_NAMES,
  sessionUser,
  type RepositoryView,
} from './mock-state'

type Field = components['schemas']['FieldError']
type RepositoryCreate = components['schemas']['RepositoryCreate']
type RepositoryPatch = components['schemas']['RepositoryPatch']

const API = '/api/v1'
const nowIso = (): string => new Date().toISOString().replace(/\.\d{3}Z$/, 'Z')
let created = 0

/**
 * Stateful mock of the repository endpoints, enforcing the contract rules the UI depends on:
 * admin only writes (403), name taken (409 `already_exists`), field errors (400 `invalid_input`
 * with JSON pointers), the SSRF filter (400 `upstream_url_rejected` with the rule), immutable
 * fields, `If-Match` (428 missing, 412 stale), group cycles (409), delete blocked by a group
 * (409 `resource_in_use` with dependents) and by content (409 `repository_not_empty`).
 *
 * Triggers for the error states without a backend: a list search for `error503` or `error429`
 * answers 503 and 429 with Retry-After, and creating a repository named `ratelimited` or
 * `unavailable` does the same; a remote URL host containing `down` makes the upstream probe fail.
 */
function forbidden(): Response {
  return problem(403, 'forbidden', 'You are not allowed to administer repositories')
}

async function readBody<T>(request: Request): Promise<T | null> {
  try {
    return (await request.json()) as T
  } catch {
    return null
  }
}

const PRIVATE_HOST = /^(localhost|127\.|10\.|192\.168\.|169\.254\.|0\.0\.0\.0)|\.internal$|\.local$/

function checkRemoteUrl(value: unknown): Response | null {
  let url: URL
  try {
    url = new URL(String(value))
  } catch {
    return invalid([
      { field: '/proxy/remote_url', code: 'invalid_url', message: 'Not a valid URL' },
    ])
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    return invalid([
      {
        field: '/proxy/remote_url',
        code: 'invalid_scheme',
        message: 'Use an http or https address',
      },
    ])
  }
  if (PRIVATE_HOST.test(url.hostname)) {
    return problem(400, 'upstream_url_rejected', 'The remote address is not allowed', {
      details: { rule: 'private_address' },
    })
  }
  return null
}

function invalid(fields: Field[]): Response {
  return problem(400, 'invalid_input', 'The request is not valid', { details: { fields } })
}

function validateGroup(members: readonly string[], format: string, self: string | null): Field[] {
  const fields: Field[] = []
  members.forEach((member, index) => {
    const found = repositories.find((r) => r.name === member)
    const pointer = `/group/members/${index}`
    if (found === undefined) {
      fields.push({
        field: pointer,
        code: 'unknown_repository',
        message: `Repository ${member} does not exist`,
      })
    } else if (found.format !== format) {
      fields.push({
        field: pointer,
        code: 'format_mismatch',
        message: `${member} has another format`,
      })
    } else if (member === self) {
      fields.push({
        field: pointer,
        code: 'self_reference',
        message: 'A group cannot contain itself',
      })
    }
  })
  return fields
}

/** True when `target` is reachable from `start` through group membership: adding it would close a cycle. */
function reaches(start: string, target: string, seen = new Set<string>()): boolean {
  if (start === target) return true
  if (seen.has(start)) return false
  seen.add(start)
  const repo = repositories.find((r) => r.name === start)
  return (repo?.group?.members ?? []).some((member) => reaches(member, target, seen))
}

function buildView(body: RepositoryCreate): RepositoryView {
  created += 1
  const format = MOCK_FORMATS.find((f) => f.format === body.format)
  const id = `0192f6c4-5a1e-7000-8000-${(0x100 + 1000 + created).toString(16).padStart(12, '0')}`
  const hosted = body.repo_type === 'hosted'
  const now = nowIso()
  return {
    id,
    name: body.name,
    format: body.format,
    repo_type: body.repo_type,
    blob_store_id: body.blob_store_id,
    write_policy: hosted ? (body.write_policy ?? format?.default_write_policy ?? 'allow') : 'deny',
    allow_anonymous: body.allow_anonymous ?? false,
    online: body.online ?? true,
    quota_bytes: body.quota_bytes ?? null,
    cleanup_policy_ids: body.cleanup_policy_ids ?? [],
    routing_rule_id: body.routing_rule_id ?? null,
    proxy: body.proxy
      ? {
          remote_url: body.proxy.remote_url,
          auth: body.proxy.auth
            ? {
                username: body.proxy.auth.username,
                password_set: body.proxy.auth.password !== undefined,
              }
            : null,
          content_max_age_secs: body.proxy.content_max_age_secs ?? 86400,
          metadata_max_age_secs: body.proxy.metadata_max_age_secs ?? 1800,
          negative_cache_ttl_secs: body.proxy.negative_cache_ttl_secs ?? 3600,
          max_concurrent_upstream: body.proxy.max_concurrent_upstream ?? 16,
        }
      : null,
    group: body.group ? { members: [...body.group.members] } : null,
    format_config: body.format_config ?? {},
    created_at: now,
    updated_at: now,
    blob_store: blobStoreRef(body.blob_store_id),
    url: `https://repo.example.org/repository/${body.name}/`,
    cleanup_policies: (body.cleanup_policy_ids ?? []).map((cid) => ({
      id: cid,
      name: CLEANUP_POLICY_NAMES[cid] ?? `policy-${cid.slice(-4)}`,
    })),
    routing_rule: body.routing_rule_id
      ? {
          id: body.routing_rule_id,
          name:
            ROUTING_RULE_NAMES[body.routing_rule_id] ?? `rule-${body.routing_rule_id.slice(-4)}`,
        }
      : null,
    stats: { component_count: 0, asset_count: 0, size_bytes: 0, as_of: now },
    health: null,
    permissions: { browse: true, read: true, write: hosted, delete: true, admin: true },
    upstream: body.proxy
      ? {
          state: 'unknown',
          blocked_until: null,
          last_error_code: null,
          last_checked_at: null,
          circuit: 'closed',
        }
      : null,
    anonymous_effective: body.allow_anonymous ?? false,
  }
}

function applyPatch(repo: RepositoryView, patch: RepositoryPatch): RepositoryView {
  const next: RepositoryView = { ...repo, updated_at: nowIso() }
  if (patch.write_policy !== undefined) next.write_policy = patch.write_policy
  if (patch.allow_anonymous !== undefined) {
    next.allow_anonymous = patch.allow_anonymous
    next.anonymous_effective = patch.allow_anonymous
  }
  if (patch.online !== undefined) next.online = patch.online
  if (patch.quota_bytes !== undefined) next.quota_bytes = patch.quota_bytes
  if (patch.cleanup_policy_ids !== undefined) {
    next.cleanup_policy_ids = patch.cleanup_policy_ids
    next.cleanup_policies = patch.cleanup_policy_ids.map((id) => ({
      id,
      name: CLEANUP_POLICY_NAMES[id] ?? `policy-${id.slice(-4)}`,
    }))
  }
  if (patch.routing_rule_id !== undefined) {
    next.routing_rule_id = patch.routing_rule_id
    next.routing_rule =
      patch.routing_rule_id === null
        ? null
        : {
            id: patch.routing_rule_id,
            name:
              ROUTING_RULE_NAMES[patch.routing_rule_id] ??
              `rule-${patch.routing_rule_id.slice(-4)}`,
          }
  }
  if (patch.group !== undefined) next.group = { members: [...patch.group.members] }
  if (patch.proxy !== undefined && repo.proxy !== null) {
    const { auth, ...rest } = patch.proxy
    const merged = { ...repo.proxy, ...rest }
    if (auth === null) merged.auth = null
    else if (auth !== undefined) {
      merged.auth = {
        username: auth.username,
        password_set: auth.password !== undefined || (repo.proxy.auth?.password_set ?? false),
      }
    }
    next.proxy = merged
  }
  if (patch.format_config !== undefined) {
    const config = { ...repo.format_config }
    for (const [key, value] of Object.entries(patch.format_config)) {
      if (value === null) delete config[key]
      else config[key] = value
    }
    next.format_config = config
  }
  return next
}

export const handlers: HttpHandler[] = [
  http.get(`${API}/repositories`, async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const q = url.searchParams.get('q')?.toLowerCase()
    if (q === 'error503')
      return problem(503, 'store_unavailable', 'The database is not reachable', {
        retryAfterSecs: 2,
      })
    if (q === 'error429')
      return problem(429, 'rate_limited', 'Too many requests', { retryAfterSecs: 2 })
    const format = url.searchParams.get('format')
    const type = url.searchParams.get('repo_type')
    const online = url.searchParams.get('online')
    const blobStore = url.searchParams.get('blob_store')
    const anonymous = url.searchParams.get('allow_anonymous')
    const sort = url.searchParams.get('sort') ?? 'name'
    const withStats = (url.searchParams.get('include') ?? '').split(',').includes('stats')
    const key = sort.replace(/^-/, '')
    if (key !== 'name' && key !== 'created_at') {
      return problem(400, 'unsupported_sort', 'The endpoint cannot sort by this field')
    }
    const rows = visibleRepositories()
      .filter(
        (r) =>
          (q === undefined || r.name.toLowerCase().includes(q)) &&
          (format === null || r.format === format) &&
          (type === null || r.repo_type === type) &&
          (online === null || String(r.online) === online) &&
          (blobStore === null || r.blob_store.name === blobStore) &&
          (anonymous === null || String(r.allow_anonymous) === anonymous),
      )
      .sort((a, b) => (a[key] < b[key] ? -1 : a[key] > b[key] ? 1 : 0))
    if (sort.startsWith('-')) rows.reverse()
    const result = paginate(
      rows.map((r) => present(r, withStats)),
      url,
    )
    return result instanceof Response ? result : json(result)
  }),

  http.get(`${API}/repositories/:name`, async ({ params }) => {
    await delay(LATENCY_MS)
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    return json(present(repo), { headers: { ETag: etagOf(repo) } })
  }),

  http.post(`${API}/repositories`, async ({ request }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const body = await readBody<RepositoryCreate>(request)
    if (body === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    if (body.name === 'ratelimited')
      return problem(429, 'rate_limited', 'Too many requests', { retryAfterSecs: 2 })
    if (body.name === 'unavailable') {
      return problem(503, 'store_unavailable', 'The database is not reachable', {
        retryAfterSecs: 2,
      })
    }
    const fields: Field[] = []
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(body.name) || body.name.includes('..')) {
      fields.push({
        field: '/name',
        code: 'invalid_name',
        message: 'The name is not a valid repository name',
      })
    }
    const format = MOCK_FORMATS.find((f) => f.format === body.format)
    if (format === undefined) {
      fields.push({ field: '/format', code: 'unknown_format', message: 'Unknown format' })
    } else if (!format.supports[body.repo_type]) {
      fields.push({
        field: '/repo_type',
        code: 'unsupported_type',
        message: `${format.label} has no ${body.repo_type} repositories`,
      })
    }
    if (body.blob_store_id === '00000000-0000-0000-0000-000000000000') {
      fields.push({
        field: '/blob_store_id',
        code: 'unknown_blob_store',
        message: 'The blob store does not exist',
      })
    }
    if (
      body.repo_type !== 'hosted' &&
      body.write_policy !== undefined &&
      body.proxy === undefined &&
      body.group === undefined
    ) {
      fields.push({
        field: '/write_policy',
        code: 'not_hosted',
        message: 'Only hosted repositories have a write policy',
      })
    }
    if (body.repo_type === 'hosted' && (body.proxy !== undefined || body.group !== undefined)) {
      fields.push({
        field: '/proxy',
        code: 'not_proxy',
        message: 'A hosted repository has no proxy or group settings',
      })
    }
    if (body.repo_type === 'proxy' && body.proxy === undefined) {
      fields.push({
        field: '/proxy',
        code: 'required',
        message: 'A proxy repository needs proxy settings',
      })
    }
    if (body.repo_type === 'group') {
      fields.push(...validateGroup(body.group?.members ?? [], body.format, null))
    }
    if (fields.length > 0) return invalid(fields)
    if (body.repo_type === 'proxy' && body.proxy) {
      const rejected = checkRemoteUrl(body.proxy.remote_url)
      if (rejected) return rejected
    }
    if (repositories.some((r) => r.name === body.name)) {
      return problem(409, 'already_exists', 'A repository with this name already exists')
    }
    const view = buildView(body)
    repositories.push(view)
    return json(present(view), {
      status: 201,
      headers: { ETag: etagOf(view), Location: `${API}/repositories/${view.name}` },
    })
  }),

  http.patch(`${API}/repositories/:name`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    const ifMatch = request.headers.get('if-match')
    if (ifMatch === null)
      return problem(428, 'precondition_required', 'The request needs an If-Match header')
    if (ifMatch === '"stale"' || ifMatch !== etagOf(repo)) {
      return problem(412, 'precondition_failed', 'The repository was changed by someone else')
    }
    const patch = await readBody<RepositoryPatch & Record<string, unknown>>(request)
    if (patch === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    for (const key of ['name', 'format', 'repo_type', 'blob_store_id'] as const) {
      if (patch[key] !== undefined && patch[key] !== repo[key]) {
        return problem(400, 'immutable_field', 'This field cannot change after creation', {
          details: {
            fields: [
              { field: `/${key}`, code: 'immutable_field', message: 'Fixed after creation' },
            ],
          },
        })
      }
    }
    if (patch.group !== undefined) {
      const fields = validateGroup(patch.group.members, repo.format, repo.name)
      if (fields.length > 0) return invalid(fields)
      if (patch.group.members.some((m) => reaches(m, repo.name))) {
        return problem(409, 'conflict', 'The new membership would make the group contain itself', {
          details: {
            dependents: patch.group.members
              .filter((m) => reaches(m, repo.name))
              .map((name) => ({ kind: 'repository', name })),
          },
        })
      }
    }
    if (patch.proxy?.remote_url !== undefined) {
      const rejected = checkRemoteUrl(patch.proxy.remote_url)
      if (rejected) return rejected
    }
    const next = applyPatch(repo, patch)
    replaceRepository(next)
    bumpVersion(next)
    return json(present(next), { headers: { ETag: etagOf(next) } })
  }),

  http.delete(`${API}/repositories/:name`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    const groups = repositories.filter((r) => r.group?.members.includes(repo.name))
    if (groups.length > 0) {
      return problem(409, 'resource_in_use', 'The repository is a member of a group', {
        details: {
          dependents: groups.map((g) => ({ kind: 'repository', id: g.id, name: g.name })),
        },
      })
    }
    const includeContent = new URL(request.url).searchParams.get('include_content') === 'true'
    const count = repo.repo_type === 'group' ? 0 : ownComponents(repo).length
    if (count > 0 && !includeContent) {
      return problem(409, 'repository_not_empty', 'The repository still holds components', {
        details: { component_count: count },
      })
    }
    repositories.splice(
      repositories.findIndex((r) => r.id === repo.id),
      1,
    )
    return new HttpResponse(null, { status: 204 })
  }),

  http.post(`${API}/upstream-checks`, async ({ request }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const body = await readBody<{ remote_url: string }>(request)
    let host = ''
    try {
      host = new URL(body?.remote_url ?? '').hostname
    } catch {
      return invalid([{ field: '/remote_url', code: 'invalid_url', message: 'Not a valid URL' }])
    }
    const result = PRIVATE_HOST.test(host)
      ? {
          ok: false,
          http_status: null,
          latency_ms: null,
          error_code: 'upstream_url_rejected',
          rule: 'private_address',
        }
      : host.includes('down')
        ? {
            ok: false,
            http_status: null,
            latency_ms: null,
            error_code: 'upstream_timeout',
            rule: null,
          }
        : { ok: true, http_status: 200, latency_ms: 143, error_code: null, rule: null }
    return json(result)
  }),

  http.post(`${API}/repositories/:name/actions/invalidate-cache`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    if (repo.repo_type !== 'proxy')
      return problem(409, 'conflict', 'Only proxy repositories have a cache')
    return json({ invalidated_at: nowIso() })
  }),

  http.post(`${API}/repositories/:name/actions/set-blocked`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    if (sessionUser()?.admin !== true) return forbidden()
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    if (repo.repo_type !== 'proxy' || repo.upstream === null) {
      return problem(409, 'conflict', 'Only proxy repositories have an upstream')
    }
    const body = await readBody<{ blocked: boolean }>(request)
    const next: RepositoryView = {
      ...repo,
      upstream: {
        ...repo.upstream,
        state: body?.blocked ? 'blocked' : 'ok',
        last_checked_at: nowIso(),
      },
    }
    replaceRepository(next)
    bumpVersion(next)
    return json(present(next), { headers: { ETag: etagOf(next) } })
  }),
]

/**
 * `GET /formats` and `GET /formats/{format}/upload-schema`. They are registered by
 * `features/browse/mocks.ts` and not here: the mock handlers are matched in the alphabetical order
 * of their feature folders, and the cleanup policy mock (`features/cleanup`) serves `/formats` too,
 * with empty option schemas. These come first so the repository form and the upload form see the
 * formats with their options and upload fields.
 */
export const formatHandlers: HttpHandler[] = [
  http.get(`${API}/formats`, async () => {
    await delay(LATENCY_MS)
    return json({ items: MOCK_FORMATS, next: null })
  }),

  http.get(`${API}/formats/:format/upload-schema`, async ({ params, request }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    const schema = MOCK_FORMATS.some((f) => f.format === params.format)
      ? mockUploadSchema(params.format as RepositoryView['format'])
      : null
    return schema === null ? problem(404, 'format_not_found', 'Unknown format') : json(schema)
  }),
]
