import { delay, http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { addComponent, existingPaths } from '@/features/components/mock-content'
import { toAssetView, toComponentView } from '@/features/components/mocks'
import { mockUploadSchema } from '@/features/repositories/mock-formats'
import { findVisible, permissionsFor, sessionUser } from '@/features/repositories/mock-state'
import { requireSession } from '@/mocks/auth-guard'
import { problem } from '@/mocks/problem'
import { LATENCY_MS } from '@/mocks/store'

type UploadResult = components['schemas']['UploadResult']
type UploadAttributes = components['schemas']['UploadAttributes']

const replays = new Map<string, UploadResult>()

/** Name and version from a file name such as `api-core-1.2.3-sources.jar` or `acme_api-2.0.0.tar.gz`. */
export function parseFileName(name: string): { name: string; version: string } {
  const match = /^(.+?)-(\d[\w.+]*)(?:-[A-Za-z]+)?\.(?:tar\.gz|tgz|whl|jar|pom|war|zip)$/.exec(name)
  return { name: match?.[1] ?? name.replace(/\.[^.]+$/, ''), version: match?.[2] ?? '0.0.0' }
}

const text = (value: unknown): string | null =>
  typeof value === 'string' && value.trim() !== '' ? value.trim() : null

function parseAttributes(raw: FormDataEntryValue | null): Promise<UploadAttributes | null> {
  if (raw === null || typeof raw === 'string')
    return Promise.resolve(raw === null ? null : safe(raw))
  return raw.text().then(safe)
}

function safe(json: string): UploadAttributes | null {
  try {
    return JSON.parse(json) as UploadAttributes
  } catch {
    return null
  }
}

/**
 * `POST /api/v1/components?repository=`: the multipart upload of the contract. It validates the
 * form against the same upload schema it serves, applies the write policy, and publishes into the
 * shared mock content, so the new component is in the tree, the list and the search at once.
 *
 * Triggers for the failure states, by file name: `quota` answers 507 with the numbers, `digest`
 * 400 `digest_mismatch`, `toolarge` 413, `unavailable` 503 with Retry-After, `ratelimited` 429.
 * A retry with the same Idempotency-Key replays the first result, as the contract says.
 */
export const handlers: HttpHandler[] = [
  http.post('/api/v1/components', async ({ request }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    const url = new URL(request.url)
    const repo = findVisible(url.searchParams.get('repository') ?? '')
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    if (repo.repo_type !== 'hosted') {
      return problem(409, 'repository_not_hosted', 'Only hosted repositories take uploads')
    }
    if (!permissionsFor(repo, sessionUser()).write) {
      return problem(403, 'forbidden', 'You are not allowed to write to this repository')
    }
    const key = request.headers.get('idempotency-key')
    const replay = key === null ? undefined : replays.get(key)
    if (replay !== undefined) {
      return HttpResponse.json(replay, { status: 201, headers: { 'Idempotent-Replayed': 'true' } })
    }

    const form = await request.formData()
    const files = form.getAll('file').filter((v): v is File => typeof v !== 'string')
    const attributes = await parseAttributes(form.get('attributes'))
    if (attributes === null || files.length === 0) {
      return problem(400, 'invalid_input', 'The upload needs attributes and at least one file')
    }
    const names = files.map((f) => f.name)
    if (names.some((n) => n.includes('toolarge'))) {
      return problem(413, 'payload_too_large', 'The file is larger than the server accepts')
    }
    if (names.some((n) => n.includes('ratelimited'))) {
      return problem(429, 'rate_limited', 'Too many requests', { retryAfterSecs: 2 })
    }
    if (names.some((n) => n.includes('unavailable'))) {
      return problem(503, 'blob_store_unavailable', 'The blob store is not reachable', {
        retryAfterSecs: 2,
      })
    }
    if (names.some((n) => n.includes('digest'))) {
      return problem(400, 'digest_mismatch', 'The checksum does not match the received bytes')
    }
    if (names.some((n) => n.includes('quota'))) {
      return problem(507, 'quota_exceeded', 'The repository quota would be exceeded', {
        details: { limit_bytes: 10 * 1024 ** 3, used_bytes: 9.9 * 1024 ** 3 },
      })
    }

    const schema = mockUploadSchema(repo.format)
    const given = attributes.fields as Record<string, unknown>
    const missing = (schema?.fields ?? [])
      .filter(
        (f) =>
          f.scope === 'component' && f.required && !f.derive_from && text(given[f.name]) === null,
      )
      .map((f) => ({
        field: `/fields/${f.name}`,
        code: 'required',
        message: `${f.label} is required`,
      }))
    if (missing.length > 0) {
      return problem(400, 'invalid_input', 'The request is not valid', {
        details: { fields: missing },
      })
    }
    if (repo.write_policy === 'deny') {
      return problem(409, 'write_policy_violation', 'The repository is read only', {
        details: { policy: 'deny' },
      })
    }

    const first = parseFileName(names[0] ?? 'file')
    const group =
      text(given.group_id) ??
      (repo.format === 'maven2'
        ? 'com.example'
        : repo.format === 'raw'
          ? text(given.directory)
          : null)
    const name =
      repo.format === 'raw' ? (names[0] ?? 'file') : (text(given.artifact_id) ?? first.name)
    const version = repo.format === 'raw' ? null : (text(given.version) ?? first.version)
    const paths = files.map((file, index) => {
      const override = text(attributes.assets?.[index]?.fields?.filename)
      switch (repo.format) {
        case 'maven2':
          return `${(group ?? 'com.example').replaceAll('.', '/')}/${name}/${version ?? ''}/${file.name}`
        case 'npm':
          return `${name}/-/${file.name}`
        case 'pypi':
          return `packages/${name}/${version ?? ''}/${file.name}`
        case 'helm':
          return `charts/${file.name}`
        case 'pub':
          return `packages/${name}/versions/${file.name}`
        default:
          return `${(group ?? '').replace(/\/+$/, '')}/${override ?? file.name}`.replace(/^\//, '')
      }
    })
    const draft = {
      group,
      name,
      version,
      attributes: given,
      files: files.map((file, index) => ({
        path: paths[index] ?? file.name,
        type: file.type || 'application/octet-stream',
        size: file.size,
      })),
    }
    if (repo.write_policy === 'allow_once' && existingPaths(repo, paths).length > 0) {
      return problem(409, 'write_policy_violation', 'A file with this path already exists', {
        details: { policy: 'allow_once' },
      })
    }
    const { component, replaced } = addComponent(repo, draft)
    const result: UploadResult = {
      component: toComponentView(component, repo),
      assets: component.assets.map((a) => toAssetView(a, repo)),
      replaced,
    }
    if (key !== null) replays.set(key, result)
    return HttpResponse.json(result, {
      status: 201,
      headers: { Location: `/api/v1/components/${component.id}` },
    })
  }),
]
