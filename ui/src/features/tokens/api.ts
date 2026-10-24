import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import type { InfinitePageConfig } from '@/api/pagination'
import { unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'

export type ApiTokenView = components['schemas']['ApiTokenView']
export type ApiTokenCreate = components['schemas']['ApiTokenCreate']
export type ApiTokenCreated = components['schemas']['ApiTokenCreated']
type Kind = 'personal' | 'robot' | 'service'
type State = 'active' | 'expired' | 'revoked' | 'disabled'

export interface MyTokenFilters {
  state?: string
}

export interface AdminTokenFilters {
  state?: string
  kind?: string
  never_used?: string
  /** Days from now: tokens that expire within that many days. */
  expiring?: string
  owner_id?: string
}

export const tokenKeys = {
  all: ['tokens'] as const,
  mine: (filters: MyTokenFilters, limit: number) =>
    [...tokenKeys.all, 'mine', filters, limit] as const,
  admin: (filters: AdminTokenFilters, limit: number) =>
    [...tokenKeys.all, 'admin', filters, limit] as const,
}

export function myTokenPage(
  filters: MyTokenFilters,
  limit: number,
): InfinitePageConfig<ApiTokenView> {
  return {
    queryKey: tokenKeys.mine(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/me/tokens', {
          params: {
            query: { limit, after: after ?? undefined, state: filters.state as State | undefined },
          },
          signal,
        }),
      ),
  }
}

export function adminTokenPage(
  filters: AdminTokenFilters,
  limit: number,
): InfinitePageConfig<ApiTokenView> {
  return {
    queryKey: tokenKeys.admin(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/api-tokens', {
          params: {
            query: {
              limit,
              after: after ?? undefined,
              state: filters.state as State | undefined,
              kind: filters.kind as Kind | undefined,
              owner_id: filters.owner_id,
              never_used: filters.never_used === 'true' ? true : undefined,
              // The instant is computed when the page is fetched, so "within 7 days" stays true.
              expiring_before:
                filters.expiring === undefined
                  ? undefined
                  : new Date(Date.now() + Number(filters.expiring) * 86_400_000).toISOString(),
            },
          },
          signal,
        }),
      ),
  }
}

/**
 * Creates a personal token. A plain function, not a mutation, on purpose: the response carries the
 * secret, and a TanStack mutation would keep it in its cache. The caller holds the result in
 * component state only until the user acknowledged it.
 */
export async function createMyToken(input: ApiTokenCreate): Promise<ApiTokenCreated> {
  return (await unwrapVersioned(api.POST('/api/v1/me/tokens', { body: input }))).data
}

export function useRevokeMyToken() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (id: string) =>
      unwrap(
        api.POST('/api/v1/me/tokens/{token_id}/actions/revoke', {
          params: { path: { token_id: id } },
        }),
      ),
    onSettled: () => queryClient.invalidateQueries({ queryKey: tokenKeys.all }),
  })
}

export function useRevokeAnyToken() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (id: string) =>
      unwrap(
        api.POST('/api/v1/api-tokens/{token_id}/actions/revoke', {
          params: { path: { token_id: id } },
        }),
      ),
    // A user's detail page shows the number of its active tokens.
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: tokenKeys.all }),
        queryClient.invalidateQueries({ queryKey: ['users'] }),
      ]),
  })
}
