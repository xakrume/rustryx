import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import type { InfinitePageConfig } from '@/api/pagination'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'
import { dashboardKeys } from '@/features/dashboard/api'
import { repositoryKeys } from '@/features/repositories/api'

export type ComponentView = components['schemas']['ComponentView']
export type ComponentDetail = components['schemas']['ComponentDetail']
export type AssetView = components['schemas']['AssetView']
export type AssetDetail = components['schemas']['AssetDetail']

export interface ComponentFilters {
  q?: string
  group?: string
  name?: string
  version?: string
  sort?: string
}

export const componentKeys = {
  all: ['components'] as const,
  list: (repository: string, filters: ComponentFilters, limit: number) =>
    [...componentKeys.all, 'list', repository, filters, limit] as const,
  detail: (id: string) => [...componentKeys.all, 'detail', id] as const,
  asset: (id: string) => [...componentKeys.all, 'asset', id] as const,
}

const SORTS = ['-created_at', 'created_at', '-updated_at', 'updated_at', 'name', '-name'] as const
type Sort = (typeof SORTS)[number]
const isSort = (value: string | undefined): value is Sort =>
  value !== undefined && (SORTS as readonly string[]).includes(value)

/** One page of the flat component list of a repository; filters and sort are part of the key. */
export function componentListConfig(
  repository: string,
  filters: ComponentFilters,
  limit: number,
): InfinitePageConfig<ComponentView> {
  return {
    queryKey: componentKeys.list(repository, filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/components', {
          params: {
            query: {
              repository,
              limit,
              after: after ?? undefined,
              q: filters.q,
              group: filters.group,
              name: filters.name,
              version: filters.version,
              sort: isSort(filters.sort) ? filters.sort : undefined,
            },
          },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  }
}

export const componentQuery = (id: string) =>
  queryOptions({
    queryKey: componentKeys.detail(id),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/components/{component_id}', {
          params: { path: { component_id: id } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  })

/** `check_blob` asks the server to verify the blob exists: one store request, so only the detail page does it. */
export const assetQuery = (id: string, checkBlob = false) =>
  queryOptions({
    queryKey: [...componentKeys.asset(id), checkBlob] as const,
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/assets/{asset_id}', {
          params: { path: { asset_id: id }, query: { check_blob: checkBlob } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  })

function useInvalidateContent() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: componentKeys.all }),
      queryClient.invalidateQueries({ queryKey: repositoryKeys.all }),
      queryClient.invalidateQueries({ queryKey: ['browse'] }),
      queryClient.invalidateQueries({ queryKey: ['search'] }),
      queryClient.invalidateQueries({ queryKey: dashboardKeys.all }),
    ])
}

/**
 * Deleting is a 204; a repeated delete answers 404, which the contract says to treat as done
 * (docs/api-conventions.md, section 9), so a second click or a deleted-meanwhile row is not an error.
 */
export function useDeleteComponent() {
  const invalidate = useInvalidateContent()
  return useMutation({
    meta: { silent: true },
    mutationFn: async (id: string) => {
      await unwrap(
        api.DELETE('/api/v1/components/{component_id}', { params: { path: { component_id: id } } }),
      )
    },
    onSuccess: () => invalidate(),
  })
}

export function useDeleteAsset() {
  const invalidate = useInvalidateContent()
  return useMutation({
    meta: { silent: true },
    mutationFn: async (id: string) => {
      await unwrap(api.DELETE('/api/v1/assets/{asset_id}', { params: { path: { asset_id: id } } }))
    },
    onSuccess: () => invalidate(),
  })
}

/** "com.example:lib", "@acme/ui", "acme/api": how the ecosystem writes a name, from the parts the API gives. */
export function componentName(c: Pick<ComponentView, 'group_id' | 'name' | 'format'>): string {
  if (c.group_id === null) return c.name
  if (c.format === 'maven2') return `${c.group_id}:${c.name}`
  if (c.format === 'npm') return `${c.group_id}/${c.name}`
  return c.name
}

export function componentLabel(
  c: Pick<ComponentView, 'group_id' | 'name' | 'format' | 'version'>,
): string {
  const name = componentName(c)
  return c.version === null ? name : `${name} ${c.version}`
}
