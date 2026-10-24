import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import type { InfinitePageConfig } from '@/api/pagination'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type AnonymousAccess = components['schemas']['AnonymousAccess']
export type RepositoryView = components['schemas']['RepositoryView']
type RepoFormat = components['schemas']['RepoFormat']

export interface RepositoryFilters {
  q?: string
  format?: string
  allow_anonymous?: string
}

export const anonymousKeys = {
  all: ['anonymous'] as const,
  state: () => [...anonymousKeys.all, 'state'] as const,
  repositories: (filters: RepositoryFilters, limit: number) =>
    [...anonymousKeys.all, 'repositories', filters, limit] as const,
}

export const anonymousQuery = queryOptions({
  queryKey: anonymousKeys.state(),
  queryFn: ({ signal }): Promise<Versioned<AnonymousAccess>> =>
    unwrapVersioned(api.GET('/api/v1/anonymous-access', { signal })),
  staleTime: STALE_TIME.live,
})

export function repositoryPage(
  filters: RepositoryFilters,
  limit: number,
): InfinitePageConfig<RepositoryView> {
  return {
    queryKey: anonymousKeys.repositories(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/repositories', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: filters.q,
              format: filters.format as RepoFormat | undefined,
              allow_anonymous:
                filters.allow_anonymous === undefined
                  ? undefined
                  : filters.allow_anonymous === 'true',
              sort: 'name',
            },
          },
          signal,
        }),
      ),
  }
}

export function updateGlobal(
  enabled: boolean,
  etag: string | null,
): Promise<Versioned<AnonymousAccess>> {
  return unwrapVersioned(
    api.PUT('/api/v1/anonymous-access', {
      params: { header: { 'If-Match': etag ?? '' } },
      body: { global_enabled: enabled },
    }),
  )
}

export interface FlagChange {
  name: string
  allow_anonymous: boolean
}

/** One transaction on the server, all or nothing; the repository list and the counts follow. */
export function useSetRepositoryFlags() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (changes: readonly FlagChange[]) =>
      unwrap(
        api.PUT('/api/v1/anonymous-access/repositories', {
          body: { repositories: [...changes] },
        }),
      ),
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: anonymousKeys.all }),
        queryClient.invalidateQueries({ queryKey: ['repositories'] }),
      ]),
  })
}
