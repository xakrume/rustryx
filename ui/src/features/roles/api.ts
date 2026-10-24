import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import type { InfinitePageConfig } from '@/api/pagination'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type RoleView = components['schemas']['RoleView']
export type RoleInput = components['schemas']['RoleInput']
export type Privilege = components['schemas']['Privilege']

export interface RoleFilters {
  q?: string
  builtin?: string
}

export const roleKeys = {
  all: ['roles'] as const,
  list: (filters: RoleFilters, limit: number) => [...roleKeys.all, 'list', filters, limit] as const,
  picker: () => [...roleKeys.all, 'picker'] as const,
  detail: (id: string) => [...roleKeys.all, 'detail', id] as const,
  effective: (id: string) => [...roleKeys.all, 'effective', id] as const,
}

export function rolePage(filters: RoleFilters, limit: number): InfinitePageConfig<RoleView> {
  return {
    queryKey: roleKeys.list(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/roles', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: filters.q,
              builtin: filters.builtin === undefined ? undefined : filters.builtin === 'true',
            },
          },
          signal,
        }),
      ),
  }
}

/** Every role, for pickers (user roles, nested roles, mapping targets). Bounded set, one page. */
export const rolePickerQuery = queryOptions({
  queryKey: roleKeys.picker(),
  queryFn: async ({ signal }) =>
    (await unwrap(api.GET('/api/v1/roles', { params: { query: { limit: 1000 } }, signal }))).items,
  staleTime: STALE_TIME.default,
})

export const roleQuery = (id: string) =>
  queryOptions({
    queryKey: roleKeys.detail(id),
    queryFn: ({ signal }): Promise<Versioned<RoleView>> =>
      unwrapVersioned(
        api.GET('/api/v1/roles/{role_id}', { params: { path: { role_id: id } }, signal }),
      ),
    staleTime: STALE_TIME.live,
  })

export const effectivePrivilegesQuery = (id: string) =>
  queryOptions({
    queryKey: roleKeys.effective(id),
    queryFn: async ({ signal }) =>
      (
        await unwrap(
          api.GET('/api/v1/roles/{role_id}/effective-privileges', {
            params: { path: { role_id: id } },
            signal,
          }),
        )
      ).items,
    staleTime: STALE_TIME.live,
  })

export function createRole(input: RoleInput): Promise<Versioned<RoleView>> {
  return unwrapVersioned(api.POST('/api/v1/roles', { body: input }))
}

export function updateRole(
  id: string,
  input: RoleInput,
  etag: string | null,
): Promise<Versioned<RoleView>> {
  return unwrapVersioned(
    api.PUT('/api/v1/roles/{role_id}', {
      params: { path: { role_id: id }, header: { 'If-Match': etag ?? '' } },
      body: input,
    }),
  )
}

export function useDeleteRole() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: ({ id, force }: { id: string; force: boolean }) =>
      unwrap(
        api.DELETE('/api/v1/roles/{role_id}', {
          params: { path: { role_id: id }, query: { force } },
        }),
      ),
    // Users and group mappings show role names too, so a deleted role invalidates them as well.
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: roleKeys.all }),
        queryClient.invalidateQueries({ queryKey: ['users'] }),
        queryClient.invalidateQueries({ queryKey: ['idp-mappings'] }),
      ]),
  })
}
