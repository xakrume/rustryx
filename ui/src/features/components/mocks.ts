import { delay, http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { paginate } from '@/features/repositories/mock-page'
import { findVisible, permissionsFor, sessionUser } from '@/features/repositories/mock-state'
import { MOCK_USERS } from '@/mocks/data'
import { requireSession } from '@/mocks/auth-guard'
import { problem } from '@/mocks/problem'
import { json, LATENCY_MS, visibleRepositories } from '@/mocks/store'
import {
  findAsset,
  findComponent,
  removeAsset,
  removeComponent,
  visibleComponents,
  type MockAsset,
  type MockComponent,
} from './mock-content'

type ComponentView = components['schemas']['ComponentView']
type ComponentDetail = components['schemas']['ComponentDetail']
type AssetView = components['schemas']['AssetView']
type AssetDetail = components['schemas']['AssetDetail']
type RepositoryView = components['schemas']['RepositoryView']

const API = '/api/v1'

/** The repository of a component or asset, or undefined when the caller may not browse it (404, never 403). */
function repositoryOf(name: string): RepositoryView | undefined {
  return findVisible(name)
}

export function toAssetView(asset: MockAsset, repo: RepositoryView): AssetView {
  return {
    id: asset.id,
    repository_id: repo.id,
    component_id: asset.componentId,
    path: asset.path,
    blob_id: `0192f6c4-5a1e-7000-8000-b${asset.id.slice(-11)}`,
    content_type: asset.contentType,
    size_bytes: asset.size,
    checksums: { sha1: asset.sha1, sha256: asset.sha256, md5: asset.md5 },
    last_downloaded_at: asset.lastDownloadedAt,
    created_at: asset.createdAt,
    updated_at: asset.createdAt,
    repository: repo.name,
    download_url: `${repo.url}${asset.path}`,
  }
}

export function toComponentView(component: MockComponent, repo: RepositoryView): ComponentView {
  const tagCount = Array.isArray(component.attributes.tags)
    ? component.attributes.tags.length
    : null
  return {
    id: component.id,
    repository_id: repo.id,
    format: component.format,
    group_id: component.group,
    name: component.name,
    version: component.version,
    created_at: component.createdAt,
    updated_at: component.createdAt,
    repository: repo.name,
    asset_count: component.assets.length,
    size_bytes: component.assets.reduce((sum, a) => sum + a.size, 0),
    attributes_summary:
      tagCount === null
        ? {}
        : {
            tag_count: tagCount,
            digest:
              typeof component.attributes.digest === 'string'
                ? component.attributes.digest
                : typeof component.attributes.digest === 'number' ||
                    typeof component.attributes.digest === 'boolean'
                  ? String(component.attributes.digest)
                  : '',
          },
  }
}

function mayDelete(repo: RepositoryView): Response | null {
  if (repo.repo_type === 'group') {
    return problem(409, 'repository_not_hosted', 'Group repositories have no content of their own')
  }
  if (!permissionsFor(repo, sessionUser()).delete) {
    return problem(403, 'forbidden', 'You are not allowed to delete content from this repository')
  }
  return null
}

const matches = (value: string | null, filter: string): boolean =>
  value !== null && value.toLowerCase().includes(filter.toLowerCase())

/**
 * Components and assets of the mock. Content comes from `mock-content.ts`, shared with the tree,
 * the search and the upload, so a delete here disappears from the tree and the search too.
 * A repository the caller may not browse answers 404 like a missing one (docs/api-conventions.md, 12).
 */
export const handlers: HttpHandler[] = [
  http.get(`${API}/components`, async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const repository = url.searchParams.get('repository')
    const group = url.searchParams.get('group')
    const name = url.searchParams.get('name')
    const version = url.searchParams.get('version')
    const q = url.searchParams.get('q')
    const sort = url.searchParams.get('sort') ?? '-created_at'
    const repos = visibleRepositories().filter((r) => repository === null || r.name === repository)
    if (repository !== null && repos.length === 0) {
      return problem(404, 'repository_not_found', 'Repository not found')
    }
    const rows = repos.flatMap((repo) =>
      visibleComponents(repo)
        .filter(
          (c) =>
            (group === null || c.group === group) &&
            (name === null || c.name === name) &&
            (version === null || c.version === version) &&
            (q === null || matches(c.group, q) || matches(c.name, q) || matches(c.version, q)),
        )
        .map((c) => toComponentView(c, findVisible(c.repository) ?? repo)),
    )
    const key = sort.replace(/^-/, '') as 'created_at' | 'updated_at' | 'name'
    rows.sort((a, b) => (a[key] < b[key] ? -1 : a[key] > b[key] ? 1 : 0))
    if (sort.startsWith('-')) rows.reverse()
    const result = paginate(rows, url)
    return result instanceof Response ? result : json(result)
  }),

  http.get(`${API}/components/:id`, async ({ params }) => {
    await delay(LATENCY_MS)
    const component = findComponent(String(params.id))
    const repo = component === undefined ? undefined : repositoryOf(component.repository)
    if (component === undefined || repo === undefined) {
      return problem(404, 'component_not_found', 'Component not found')
    }
    const detail: ComponentDetail = {
      ...toComponentView(component, repo),
      assets: component.assets.map((a) => toAssetView(a, repo)),
      assets_truncated: false,
      attributes: component.attributes,
    }
    return json(detail, {
      headers: { ETag: `"component-${component.id}-${component.assets.length}"` },
    })
  }),

  http.delete(`${API}/components/:id`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    const component = findComponent(String(params.id))
    const repo = component === undefined ? undefined : repositoryOf(component.repository)
    if (component === undefined || repo === undefined) {
      return problem(404, 'component_not_found', 'Component not found')
    }
    const refused = mayDelete(repo)
    if (refused) return refused
    removeComponent(component)
    return new HttpResponse(null, { status: 204 })
  }),

  http.get(`${API}/assets`, async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const repository = url.searchParams.get('repository')
    const componentId = url.searchParams.get('component_id')
    const prefix = url.searchParams.get('prefix') ?? ''
    const sha256 = url.searchParams.get('sha256')
    const sha1 = url.searchParams.get('sha1')
    const md5 = url.searchParams.get('md5')
    const repos = visibleRepositories().filter((r) => repository === null || r.name === repository)
    if (repository !== null && repos.length === 0) {
      return problem(404, 'repository_not_found', 'Repository not found')
    }
    const rows = repos
      .flatMap((repo) =>
        visibleComponents(repo).flatMap((c) =>
          c.assets.map((a) => ({ asset: a, repo: findVisible(c.repository) ?? repo })),
        ),
      )
      .filter(
        ({ asset }) =>
          asset.path.startsWith(prefix) &&
          (componentId === null || asset.componentId === componentId) &&
          (sha256 === null || asset.sha256 === sha256) &&
          (sha1 === null || asset.sha1 === sha1) &&
          (md5 === null || asset.md5 === md5),
      )
      .sort((a, b) => (a.asset.path < b.asset.path ? -1 : a.asset.path > b.asset.path ? 1 : 0))
      .map(({ asset, repo }) => toAssetView(asset, repo))
    const result = paginate(rows, url)
    return result instanceof Response ? result : json(result)
  }),

  http.get(`${API}/assets/:id`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const asset = findAsset(String(params.id))
    const repo = asset === undefined ? undefined : repositoryOf(asset.repository)
    if (asset === undefined || repo === undefined) {
      return problem(404, 'asset_not_found', 'Asset not found')
    }
    const check = new URL(request.url).searchParams.get('check_blob') === 'true'
    const admin = sessionUser()?.admin === true
    const uploader = MOCK_USERS[0]
    const detail: AssetDetail = {
      ...toAssetView(asset, repo),
      attributes: { 'content-type': asset.contentType, 'blob-ref': asset.sha256.slice(0, 16) },
      download_count_30d: asset.downloads,
      uploader:
        uploader === undefined || asset.downloads % 2 === 1
          ? null
          : {
              id: uploader.id,
              username: uploader.username,
              display_name: uploader.display_name,
              source: uploader.source,
            },
      blob: {
        id: `0192f6c4-5a1e-7000-8000-b${asset.id.slice(-11)}`,
        blob_store: repo.blob_store.name,
        key: admin
          ? `content/${asset.sha256.slice(0, 2)}/${asset.sha256.slice(2, 4)}/${asset.sha256}`
          : null,
      },
      blob_state: check ? (asset.path.includes('missing') ? 'missing' : 'ok') : 'unknown',
    }
    return json(detail, { headers: { ETag: `"asset-${asset.id}"` } })
  }),

  http.delete(`${API}/assets/:id`, async ({ request, params }) => {
    await delay(LATENCY_MS)
    const guard = requireSession(request)
    if (guard instanceof Response) return guard
    const asset = findAsset(String(params.id))
    const repo = asset === undefined ? undefined : repositoryOf(asset.repository)
    if (asset === undefined || repo === undefined)
      return problem(404, 'asset_not_found', 'Asset not found')
    const refused = mayDelete(repo)
    if (refused) return refused
    removeAsset(asset)
    return new HttpResponse(null, { status: 204 })
  }),
]
