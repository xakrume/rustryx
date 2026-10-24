import { delay, http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { toAssetView, toComponentView } from '@/features/components/mocks'
import { ownComponents } from '@/features/components/mock-content'
import { paginate } from '@/features/repositories/mock-page'
import { problem } from '@/mocks/problem'
import { json, LATENCY_MS, visibleRepositories } from '@/mocks/store'

type ComponentView = components['schemas']['ComponentView']

const MAX_HITS = 1000
const MIN_PREFIX = 3

/** Exact match, or a prefix match when the filter ends in `*` (the contract's wildcard rule). */
function fieldMatches(value: string | null, filter: string | null): boolean {
  if (filter === null) return true
  if (value === null) return false
  return filter.endsWith('*') ? value.startsWith(filter.slice(0, -1)) : value === filter
}

function keyword(q: string | null, haystack: string): boolean | Response {
  if (q === null) return true
  const text = haystack.toLowerCase()
  if (q.length > 1 && q.startsWith('"') && q.endsWith('"'))
    return text.includes(q.slice(1, -1).toLowerCase())
  if (q.endsWith('*')) {
    const prefix = q.slice(0, -1)
    if (prefix.length < MIN_PREFIX) {
      return problem(400, 'query_too_short', 'The wildcard prefix is too short', {
        details: { min_prefix: MIN_PREFIX },
      })
    }
    return text.split(/[\s:/@]+/).some((word) => word.startsWith(prefix.toLowerCase()))
  }
  return q
    .toLowerCase()
    .split(/\s+/)
    .every((term) => text.includes(term))
}

/**
 * `GET /search/components` and `GET /search/assets` over the same content the tree shows. It keeps
 * the contract's rules: a query needs a criterion (400 `invalid_query`), a wildcard needs three
 * characters (400 `query_too_short` with `min_prefix`), checksums are exact, the reachable hits are
 * capped at 1000 (`truncated`), and only repositories the caller may browse are searched.
 */
interface Criteria {
  q: string | null
  format: string | null
  repository: string | null
  group: string | null
  name: string | null
  version: string | null
  sha256: string | null
  sha1: string | null
  md5: string | null
  path: string | null
}

function criteriaOf(url: URL): Criteria {
  const get = (key: string): string | null => url.searchParams.get(key)
  return {
    q: get('q'),
    format: get('format'),
    repository: get('repository'),
    group: get('group'),
    name: get('name'),
    version: get('version'),
    sha256: get('sha256'),
    sha1: get('sha1'),
    md5: get('md5'),
    path: get('path'),
  }
}

export const handlers: HttpHandler[] = [
  http.get('/api/v1/search/components', async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const c = criteriaOf(url)
    if (Object.values(c).every((v) => v === null)) {
      return problem(400, 'invalid_query', 'A search needs at least one criterion')
    }
    const sort = url.searchParams.get('sort') ?? 'relevance'
    const rows: ComponentView[] = []
    for (const repo of visibleRepositories()) {
      if (repo.repo_type === 'group') continue
      if (c.repository !== null && repo.name !== c.repository) continue
      if (c.format !== null && repo.format !== c.format) continue
      for (const component of ownComponents(repo)) {
        const hit = keyword(
          c.q,
          [component.group, component.name, component.version].filter(Boolean).join(' '),
        )
        if (hit instanceof Response) return hit
        const checksum = [c.sha256, c.sha1, c.md5].find((v) => v !== null) ?? null
        const checksumOk =
          checksum === null ||
          component.assets.some(
            (a) => a.sha256 === checksum || a.sha1 === checksum || a.md5 === checksum,
          )
        if (
          hit &&
          checksumOk &&
          fieldMatches(component.group, c.group) &&
          fieldMatches(component.name, c.name) &&
          fieldMatches(component.version, c.version)
        ) {
          rows.push(toComponentView(component, repo))
        }
      }
    }
    const key = sort.replace(/^-/, '') as 'created_at' | 'name'
    if (sort === 'relevance') rows.sort((a, b) => (a.name < b.name ? -1 : 1))
    else rows.sort((a, b) => (a[key] < b[key] ? -1 : a[key] > b[key] ? 1 : 0))
    if (sort.startsWith('-')) rows.reverse()
    const capped = rows.slice(0, MAX_HITS)
    const result = paginate(capped, url, { limit: 50, max: 200 })
    return result instanceof Response
      ? result
      : json({ ...result, truncated: rows.length >= MAX_HITS })
  }),

  http.get('/api/v1/search/assets', async ({ request }) => {
    await delay(LATENCY_MS)
    const url = new URL(request.url)
    const c = criteriaOf(url)
    if (Object.values(c).every((v) => v === null)) {
      return problem(400, 'invalid_query', 'A search needs at least one criterion')
    }
    const sort = url.searchParams.get('sort') ?? 'relevance'
    const rows = []
    for (const repo of visibleRepositories()) {
      if (repo.repo_type === 'group') continue
      if (c.repository !== null && repo.name !== c.repository) continue
      if (c.format !== null && repo.format !== c.format) continue
      for (const component of ownComponents(repo)) {
        if (
          !fieldMatches(component.group, c.group) ||
          !fieldMatches(component.name, c.name) ||
          !fieldMatches(component.version, c.version)
        ) {
          continue
        }
        for (const asset of component.assets) {
          const hit = keyword(
            c.q,
            `${component.group ?? ''} ${component.name} ${component.version ?? ''} ${asset.path}`,
          )
          if (hit instanceof Response) return hit
          if (
            hit &&
            fieldMatches(asset.path, c.path) &&
            (c.sha256 === null || asset.sha256 === c.sha256) &&
            (c.sha1 === null || asset.sha1 === c.sha1) &&
            (c.md5 === null || asset.md5 === c.md5)
          ) {
            rows.push(toAssetView(asset, repo))
          }
        }
      }
    }
    rows.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
    if (sort === '-path') rows.reverse()
    const capped = rows.slice(0, MAX_HITS)
    const result = paginate(capped, url, { limit: 50, max: 200 })
    return result instanceof Response
      ? result
      : json({ ...result, truncated: rows.length >= MAX_HITS })
  }),
]
