import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import type { InfinitePageConfig } from '@/api/pagination'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type UserView = components['schemas']['UserView']
export type UserCreate = components['schemas']['UserCreate']
export type UserPatch = components['schemas']['UserPatch']
export type ApiTokenView = components['schemas']['ApiTokenView']
type Source = components['schemas']['UserSource']

export interface UserFilters {
  q?: string
  source?: string
  enabled?: string
  role?: string
}

export const userKeys = {
  all: ['users'] as const,
  list: (filters: UserFilters, sort: string | undefined, limit: number) =>
    [...userKeys.all, 'list', filters, sort, limit] as const,
  detail: (id: string) => [...userKeys.all, 'detail', id] as const,
  tokens: (id: string) => [...userKeys.all, 'tokens', id] as const,
  admins: () => [...userKeys.all, 'admins'] as const,
}

export function userPage(
  filters: UserFilters,
  sort: string | undefined,
  limit: number,
): InfinitePageConfig<UserView> {
  return {
    queryKey: userKeys.list(filters, sort, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/users', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              q: filters.q,
              source: filters.source as Source | undefined,
              enabled: filters.enabled === undefined ? undefined : filters.enabled === 'true',
              role: filters.role,
              sort: sort as 'username' | '-username' | 'created_at' | '-created_at' | undefined,
            },
          },
          signal,
        }),
      ),
  }
}

export const userQuery = (id: string) =>
  queryOptions({
    queryKey: userKeys.detail(id),
    queryFn: ({ signal }): Promise<Versioned<UserView>> =>
      unwrapVersioned(
        api.GET('/api/v1/users/{user_id}', { params: { path: { user_id: id } }, signal }),
      ),
    staleTime: STALE_TIME.live,
  })

/** Local accounts that hold `nx-admin`: what the last administrator guard counts. */
export const localAdminsQuery = queryOptions({
  queryKey: userKeys.admins(),
  queryFn: async ({ signal }) =>
    (
      await unwrap(
        api.GET('/api/v1/users', {
          params: { query: { role: 'nx-admin', source: 'local', limit: 3 } },
          signal,
        }),
      )
    ).items,
  staleTime: STALE_TIME.live,
})

/** The first tokens of one user, for the user detail page. */
export const userTokensQuery = (id: string) =>
  queryOptions({
    queryKey: userKeys.tokens(id),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/api-tokens', { params: { query: { owner_id: id, limit: 5 } }, signal }),
      ),
    staleTime: STALE_TIME.default,
  })

export function createUser(input: UserCreate): Promise<Versioned<UserView>> {
  return unwrapVersioned(api.POST('/api/v1/users', { body: input }))
}

/** JSON Merge Patch: only the fields in `patch` change. */
export function patchUser(
  id: string,
  patch: UserPatch,
  etag: string | null,
): Promise<Versioned<UserView>> {
  return unwrapVersioned(
    api.PATCH('/api/v1/users/{user_id}', {
      params: { path: { user_id: id }, header: { 'If-Match': etag ?? '' } },
      headers: { 'Content-Type': 'application/merge-patch+json' },
      body: patch,
    }),
  )
}

/** Replaces the MANUAL roles only; roles that come from groups are untouched by the server. */
export function setUserRoles(
  id: string,
  roleIds: readonly string[],
  etag: string | null,
): Promise<Versioned<UserView>> {
  return unwrapVersioned(
    api.PUT('/api/v1/users/{user_id}/roles', {
      params: { path: { user_id: id }, header: { 'If-Match': etag ?? '' } },
      body: { role_ids: [...roleIds] },
    }),
  )
}

export function useResetPassword(id: string) {
  return useMutation({
    meta: { silent: true },
    mutationFn: (password: string) =>
      unwrap(
        api.POST('/api/v1/users/{user_id}/actions/reset-password', {
          params: { path: { user_id: id } },
          body: { password },
        }),
      ),
  })
}

export function useSignOutUser(id: string) {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: () =>
      unwrap(
        api.POST('/api/v1/users/{user_id}/actions/sign-out', { params: { path: { user_id: id } } }),
      ),
    onSettled: () => queryClient.invalidateQueries({ queryKey: userKeys.detail(id) }),
  })
}

export function useDeleteUser() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (id: string) =>
      unwrap(api.DELETE('/api/v1/users/{user_id}', { params: { path: { user_id: id } } })),
    onSettled: () => queryClient.invalidateQueries({ queryKey: userKeys.all }),
  })
}
