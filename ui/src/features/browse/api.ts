import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type TreeEntry = components['schemas']['TreeNode']
export type TreePage = components['schemas']['TreePage']

/** Entries per folder request. The contract allows 500; 200 is its default and keeps a folder light. */
export const FOLDER_PAGE_SIZE = 200

export const browseKeys = {
  all: ['browse'] as const,
  folder: (repository: string, path: string, q: string | undefined, after: string | null) =>
    [...browseKeys.all, repository, path, q ?? '', after ?? ''] as const,
  byPath: (repository: string, path: string) =>
    [...browseKeys.all, 'asset-by-path', repository, path] as const,
}

export function fetchFolder(
  repository: string,
  path: string,
  q: string | undefined,
  after: string | null,
  signal?: AbortSignal,
): Promise<TreePage> {
  return unwrap(
    api.GET('/api/v1/repositories/{name}/tree', {
      params: {
        path: { name: repository },
        query: { path, q, after: after ?? undefined, limit: FOLDER_PAGE_SIZE },
      },
      signal,
    }),
  )
}

/** One level of a folder, shared by the page's own probe and the tree, so a folder is fetched once. */
export const folderQuery = (
  repository: string,
  path: string,
  q: string | undefined,
  after: string | null = null,
) =>
  queryOptions({
    queryKey: browseKeys.folder(repository, path, q, after),
    queryFn: ({ signal }) => fetchFolder(repository, path, q, after, signal),
    staleTime: STALE_TIME.default,
    retry: false,
  })

/**
 * A selection that came from the address bar knows its path, not its asset id: the exact path is
 * found through the asset list (`prefix` is a prefix, so the first entry is matched exactly).
 */
export const assetByPathQuery = (repository: string, path: string, enabled: boolean) =>
  queryOptions({
    queryKey: browseKeys.byPath(repository, path),
    queryFn: async ({ signal }) => {
      const prefix = path.replace(/^\//, '')
      const page = await unwrap(
        api.GET('/api/v1/assets', {
          params: { query: { repository, prefix, limit: 5 } },
          signal,
        }),
      )
      return page.items.find((asset) => asset.path === prefix) ?? null
    },
    enabled,
    staleTime: STALE_TIME.default,
  })
