import type { InfinitePageConfig, Page } from '@/api/pagination'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type ComponentView = components['schemas']['ComponentView']
export type AssetView = components['schemas']['AssetView']
type RepoFormat = components['schemas']['RepoFormat']

export type SearchKind = 'components' | 'assets'

/** What the address bar holds: the criteria of one search, as the user typed them. */
export interface SearchCriteria {
  kind: SearchKind
  q?: string
  format?: string
  repository?: string
  group?: string
  name?: string
  version?: string
  checksum?: string
  path?: string
  sort?: string
}

export const COMPONENT_SORTS = ['relevance', '-created_at', 'name', '-name'] as const
export const ASSET_SORTS = ['relevance', 'path', '-path'] as const

/** A search needs at least one criterion (docs/api-conventions.md, section 7); the sort is not one. */
export function hasCriteria(c: SearchCriteria): boolean {
  return [c.q, c.format, c.repository, c.group, c.name, c.version, c.checksum, c.path].some(
    (value) => value !== undefined && value !== '',
  )
}

const HEX = /^[0-9a-f]+$/

/** Which digest a pasted checksum is, by its length; anything else is not a checksum. */
export function checksumParam(value: string | undefined): {
  sha256?: string
  sha1?: string
  md5?: string
} {
  const v = value?.trim().toLowerCase()
  if (v === undefined || !HEX.test(v)) return {}
  if (v.length === 64) return { sha256: v }
  if (v.length === 40) return { sha1: v }
  if (v.length === 32) return { md5: v }
  return {}
}

export function isChecksum(value: string): boolean {
  return Object.keys(checksumParam(value)).length > 0
}

function pick<T extends string>(value: string | undefined, allowed: readonly T[]): T | undefined {
  return allowed.find((candidate) => candidate === value)
}

type Hits<T> = Page<T> & { truncated: boolean }

export function componentSearchConfig(
  c: SearchCriteria,
  limit: number,
): InfinitePageConfig<ComponentView> {
  return {
    queryKey: ['search', 'components', c, limit],
    enabled: c.kind === 'components' && hasCriteria(c),
    fetchPage: ({ after, signal }): Promise<Hits<ComponentView>> =>
      unwrap(
        api.GET('/api/v1/search/components', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: c.q,
              format: c.format as RepoFormat | undefined,
              repository: c.repository,
              group: c.group,
              name: c.name,
              version: c.version,
              ...checksumParam(c.checksum),
              sort: pick(c.sort, COMPONENT_SORTS),
            },
          },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  }
}

export function assetSearchConfig(c: SearchCriteria, limit: number): InfinitePageConfig<AssetView> {
  return {
    queryKey: ['search', 'assets', c, limit],
    enabled: c.kind === 'assets' && hasCriteria(c),
    fetchPage: ({ after, signal }): Promise<Hits<AssetView>> =>
      unwrap(
        api.GET('/api/v1/search/assets', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: c.q,
              format: c.format as RepoFormat | undefined,
              repository: c.repository,
              group: c.group,
              name: c.name,
              version: c.version,
              path: c.path,
              ...checksumParam(c.checksum),
              sort: pick(c.sort, ASSET_SORTS),
            },
          },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  }
}
