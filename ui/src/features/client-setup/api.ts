import { queryOptions, useMutation } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type ClientSetup = components['schemas']['ClientSetup']
export type ClientSnippet = components['schemas']['ClientSnippet']
export type ApiTokenCreate = components['schemas']['ApiTokenCreate']

export const clientSetupKeys = {
  all: ['client-setup'] as const,
  repository: (name: string) => [...clientSetupKeys.all, name] as const,
}

/** Snippets are built by the server from templates next to the format crates (inventory, 9). */
export const clientSetupQuery = (repository: string) =>
  queryOptions({
    queryKey: clientSetupKeys.repository(repository),
    queryFn: ({ signal }) =>
      unwrap(api.GET('/api/v1/client-setup', { params: { query: { repository } }, signal })),
    staleTime: STALE_TIME.default,
  })

/**
 * "Create a token for me". The secret is in the response once and nowhere else, so this
 * mutation is not kept after it settles (`gcTime: 0`): the mutation cache must not hold a
 * token. It is silent because the dialog shows the failure itself.
 */
export function useCreateClientToken() {
  return useMutation({
    meta: { silent: true },
    gcTime: 0,
    mutationFn: (body: ApiTokenCreate) => unwrap(api.POST('/api/v1/me/tokens', { body })),
  })
}
