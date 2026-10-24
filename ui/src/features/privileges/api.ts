import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import type { InfinitePageConfig } from '@/api/pagination'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type PrivilegeView = components['schemas']['PrivilegeView']
export type PrivilegeInput = components['schemas']['PrivilegeInput']
export type PrivilegeGrant = components['schemas']['PrivilegeGrant']
export type Action = components['schemas']['Action']
type Kind = 'admin' | 'repository-view' | 'repository-content-selector'

export interface PrivilegeFilters {
  q?: string
  kind?: string
  repository?: string
  builtin?: string
}

export const privilegeKeys = {
  all: ['privileges'] as const,
  list: (filters: PrivilegeFilters, limit: number) =>
    [...privilegeKeys.all, 'list', filters, limit] as const,
  picker: () => [...privilegeKeys.all, 'picker'] as const,
  detail: (id: string) => [...privilegeKeys.all, 'detail', id] as const,
}

export function privilegePage(
  filters: PrivilegeFilters,
  limit: number,
): InfinitePageConfig<PrivilegeView> {
  return {
    queryKey: privilegeKeys.list(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/privileges', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: filters.q,
              kind: filters.kind as Kind | undefined,
              repository: filters.repository,
              builtin: filters.builtin === undefined ? undefined : filters.builtin === 'true',
            },
          },
          signal,
        }),
      ),
  }
}

/** Every privilege, for pickers: the set is bounded and the API returns it in one page. */
export const privilegePickerQuery = queryOptions({
  queryKey: privilegeKeys.picker(),
  queryFn: async ({ signal }) =>
    (await unwrap(api.GET('/api/v1/privileges', { params: { query: { limit: 1000 } }, signal })))
      .items,
  staleTime: STALE_TIME.default,
})

export const privilegeQuery = (id: string) =>
  queryOptions({
    queryKey: privilegeKeys.detail(id),
    queryFn: ({ signal }): Promise<Versioned<PrivilegeView>> =>
      unwrapVersioned(
        api.GET('/api/v1/privileges/{privilege_id}', {
          params: { path: { privilege_id: id } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.live,
  })

export function createPrivilege(input: PrivilegeInput): Promise<Versioned<PrivilegeView>> {
  return unwrapVersioned(api.POST('/api/v1/privileges', { body: input }))
}

export function updatePrivilege(
  id: string,
  input: PrivilegeInput,
  etag: string | null,
): Promise<Versioned<PrivilegeView>> {
  return unwrapVersioned(
    api.PUT('/api/v1/privileges/{privilege_id}', {
      params: { path: { privilege_id: id }, header: { 'If-Match': etag ?? '' } },
      body: input,
    }),
  )
}

export function useDeletePrivilege() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: ({ id, force }: { id: string; force: boolean }) =>
      unwrap(
        api.DELETE('/api/v1/privileges/{privilege_id}', {
          params: { path: { privilege_id: id }, query: { force } },
        }),
      ),
    onSettled: () => queryClient.invalidateQueries({ queryKey: privilegeKeys.all }),
  })
}

/** Roles show their privileges and users show their roles: a privilege change touches both. */
export function invalidateRbac(queryClient: ReturnType<typeof useQueryClient>): Promise<unknown> {
  return Promise.all([
    queryClient.invalidateQueries({ queryKey: ['privileges'] }),
    queryClient.invalidateQueries({ queryKey: ['roles'] }),
  ])
}

export type RepoFormat = components['schemas']['RepoFormat']
export interface RepositoryChoice {
  name: string
  format: RepoFormat
  repo_type: components['schemas']['RepoType']
}

/** Every repository the caller may browse, for the pickers of this feature group (one request). */
export const repositoryChoicesQuery = queryOptions({
  queryKey: ['repositories', 'choices'] as const,
  queryFn: async ({ signal }): Promise<RepositoryChoice[]> =>
    (
      await unwrap(
        api.GET('/api/v1/repositories', {
          params: { query: { limit: 1000, sort: 'name' } },
          signal,
        }),
      )
    ).items.map((r) => ({ name: r.name, format: r.format, repo_type: r.repo_type })),
  staleTime: STALE_TIME.default,
})

export const REPO_FORMATS: readonly RepoFormat[] = [
  'maven2',
  'npm',
  'pypi',
  'docker',
  'oci',
  'helm',
  'raw',
  'pub',
]
export const ACTIONS: readonly Action[] = ['browse', 'read', 'write', 'delete']
