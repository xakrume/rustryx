import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import type { InfinitePageConfig, Page } from '@/api/pagination'
import { STALE_TIME } from '@/api/query-client'
import { ifMatch, unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'
import { clientSetupKeys } from '@/features/client-setup/api'
import { dashboardKeys } from '@/features/dashboard/api'

export type RepositoryView = components['schemas']['RepositoryView']
export type RepositoryCreate = components['schemas']['RepositoryCreate']
export type RepositoryPatch = components['schemas']['RepositoryPatch']
export type FormatInfo = components['schemas']['FormatInfo']
export type BlobStoreView = components['schemas']['BlobStoreView']
export type CleanupPolicyView = components['schemas']['CleanupPolicyView']
export type RoutingRuleView = components['schemas']['RoutingRuleView']
export type UpstreamCheckResult = components['schemas']['UpstreamCheckResult']
export type RepoFormat = components['schemas']['RepoFormat']
export type RepoType = components['schemas']['RepoType']
export type AuditEvent = components['schemas']['AuditEvent']

export interface RepositoryFilters {
  q?: string
  format?: string
  repo_type?: string
  online?: string
  sort?: string
}

export const repositoryKeys = {
  all: ['repositories'] as const,
  list: (filters: RepositoryFilters, limit: number) =>
    [...repositoryKeys.all, 'list', filters, limit] as const,
  detail: (name: string) => [...repositoryKeys.all, 'detail', name] as const,
  lookup: (q: string) => [...repositoryKeys.all, 'lookup', q] as const,
  names: () => [...repositoryKeys.all, 'names'] as const,
  recent: (name: string) => [...repositoryKeys.all, 'recent', name] as const,
  activity: (name: string) => [...repositoryKeys.all, 'activity', name] as const,
}

const isFormat = (value: string | undefined): value is RepoFormat =>
  value !== undefined && /^[a-z0-9]+$/.test(value)
const isType = (value: string | undefined): value is RepoType =>
  value === 'hosted' || value === 'proxy' || value === 'group'
const isSort = (
  value: string | undefined,
): value is 'name' | '-name' | 'created_at' | '-created_at' =>
  value === 'name' || value === '-name' || value === 'created_at' || value === '-created_at'

/** One page of the repository list. Filters and sort are in the key: the page token is bound to them. */
export function repositoryListConfig(
  filters: RepositoryFilters,
  limit: number,
): InfinitePageConfig<RepositoryView> {
  return {
    queryKey: repositoryKeys.list(filters, limit),
    fetchPage: ({ after, signal }): Promise<Page<RepositoryView>> =>
      unwrap(
        api.GET('/api/v1/repositories', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              include: ['stats'],
              q: filters.q,
              format: isFormat(filters.format) ? filters.format : undefined,
              repo_type: isType(filters.repo_type) ? filters.repo_type : undefined,
              online:
                filters.online === 'true' ? true : filters.online === 'false' ? false : undefined,
              sort: isSort(filters.sort) ? filters.sort : undefined,
            },
          },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  }
}

/** A repository with its ETag, so an edit can send `If-Match` (docs/api-conventions.md, 8). */
export const repositoryQuery = (name: string) =>
  queryOptions({
    queryKey: repositoryKeys.detail(name),
    queryFn: ({ signal }) =>
      unwrapVersioned(
        api.GET('/api/v1/repositories/{name}', {
          params: { path: { name }, query: { include: ['stats'] } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  })

export const fetchRepository = (name: string) =>
  unwrapVersioned(
    api.GET('/api/v1/repositories/{name}', {
      params: { path: { name }, query: { include: ['stats'] } },
    }),
  )

/** Every repository name and format, for pickers (group members, the upload target, the search filter). */
export const repositoryNamesQuery = queryOptions({
  queryKey: repositoryKeys.names(),
  queryFn: ({ signal }) =>
    unwrap(api.GET('/api/v1/repositories', { params: { query: { limit: 1000 } }, signal })),
  staleTime: STALE_TIME.default,
})

/** Live name check of the create form: the list filter is a contains search, so match exactly here. */
export const repositoryLookupQuery = (name: string) =>
  queryOptions({
    queryKey: repositoryKeys.lookup(name),
    queryFn: async ({ signal }) => {
      const page = await unwrap(
        api.GET('/api/v1/repositories', { params: { query: { q: name, limit: 50 } }, signal }),
      )
      return page.items.some((r) => r.name === name)
    },
    staleTime: 0,
  })

export const formatsQuery = queryOptions({
  queryKey: ['formats'] as const,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/formats', { signal })),
  staleTime: STALE_TIME.static,
})

export const blobStoresQuery = queryOptions({
  queryKey: ['blob-stores', 'picker'] as const,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/blob-stores', { signal })),
  staleTime: STALE_TIME.default,
})

export const cleanupPoliciesQuery = queryOptions({
  queryKey: ['cleanup-policies', 'picker'] as const,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/cleanup-policies', { signal })),
  staleTime: STALE_TIME.default,
})

export const routingRulesQuery = queryOptions({
  queryKey: ['routing-rules', 'picker'] as const,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/routing-rules', { signal })),
  staleTime: STALE_TIME.default,
})

export const recentComponentsQuery = (name: string) =>
  queryOptions({
    queryKey: repositoryKeys.recent(name),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/components', {
          params: { query: { repository: name, limit: 5, sort: '-created_at' } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  })

/** Audit events of one repository (`repository=` filter), newest first. */
export const activityConfig = (name: string): InfinitePageConfig<AuditEvent> => ({
  queryKey: repositoryKeys.activity(name),
  fetchPage: ({ after, signal }) =>
    unwrap(
      api.GET('/api/v1/audit-events', {
        params: { query: { repository: name, limit: 50, after: after ?? undefined } },
        signal,
      }),
    ),
  staleTime: STALE_TIME.live,
})

export function checkUpstream(body: components['schemas']['UpstreamCheckRequest']) {
  return unwrap(api.POST('/api/v1/upstream-checks', { body }))
}

/** PATCH of the mutable fields with the ETag of the version that was read. */
export function patchRepository(name: string, body: RepositoryPatch, etag: string | null) {
  return unwrapVersioned(
    api.PATCH('/api/v1/repositories/{name}', {
      // The generated type wants the header as a required string; `ifMatch` leaves it out when
      // there is no ETag, and the server then answers 428 instead of guessing a version.
      params: { path: { name }, header: ifMatch(etag) as { 'If-Match': string } },
      body,
      headers: { 'Content-Type': 'application/merge-patch+json' },
    }),
  )
}

function useInvalidateRepositories() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: repositoryKeys.all }),
      queryClient.invalidateQueries({ queryKey: dashboardKeys.all }),
      queryClient.invalidateQueries({ queryKey: clientSetupKeys.all }),
    ])
}

export function useCreateRepository() {
  const invalidate = useInvalidateRepositories()
  return useMutation({
    // The form maps field errors itself and shows the rest inline.
    meta: { silent: true },
    mutationFn: (body: RepositoryCreate) =>
      unwrapVersioned(api.POST('/api/v1/repositories', { body })),
    onSuccess: () => invalidate(),
  })
}

export function useDeleteRepository() {
  const invalidate = useInvalidateRepositories()
  return useMutation({
    meta: { silent: true },
    mutationFn: async ({ name, includeContent }: { name: string; includeContent: boolean }) => {
      await unwrap(
        api.DELETE('/api/v1/repositories/{name}', {
          params: { path: { name }, query: { include_content: includeContent } },
        }),
      )
    },
    onSuccess: () => invalidate(),
  })
}

export function useRepositoryAction(name: string) {
  const queryClient = useQueryClient()
  const refresh = () => queryClient.invalidateQueries({ queryKey: repositoryKeys.detail(name) })
  return {
    invalidateCache: useMutation({
      meta: { silent: true },
      mutationFn: () =>
        unwrap(
          api.POST('/api/v1/repositories/{name}/actions/invalidate-cache', {
            params: { path: { name } },
          }),
        ),
      onSuccess: refresh,
    }),
    setBlocked: useMutation({
      meta: { silent: true },
      mutationFn: (blocked: boolean) =>
        unwrap(
          api.POST('/api/v1/repositories/{name}/actions/set-blocked', {
            params: { path: { name } },
            body: { blocked },
          }),
        ),
      onSuccess: refresh,
    }),
  }
}
