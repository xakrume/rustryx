import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import { networkError, toApiError } from '@/api/errors'
import type { InfinitePageConfig } from '@/api/pagination'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type MappingView = components['schemas']['IdpGroupMappingView']
export type MappingCreate = components['schemas']['IdpGroupMappingCreate']
export type MappingUpdate = components['schemas']['IdpGroupMappingUpdate']
export type MappingTestResult = components['schemas']['MappingTestResult']
export type Provider = components['schemas']['IdpProvider']
export type AuthProvidersDetails = components['schemas']['AuthProvidersDetails']

export const mappingKeys = {
  all: ['idp-mappings'] as const,
  list: (provider: Provider, limit: number) =>
    [...mappingKeys.all, 'list', provider, limit] as const,
  providers: () => [...mappingKeys.all, 'providers'] as const,
}

export function mappingPage(provider: Provider, limit: number): InfinitePageConfig<MappingView> {
  return {
    queryKey: mappingKeys.list(provider, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/idp-group-mappings', {
          params: { query: { provider, limit, after: after ?? undefined } },
          signal,
        }),
      ),
  }
}

/** The authentication configuration (read only): the admin groups the page explains. */
export const providersDetailsQuery = queryOptions({
  queryKey: mappingKeys.providers(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/auth/providers/details', { signal })),
  staleTime: STALE_TIME.default,
})

export function createMapping(input: MappingCreate): Promise<Versioned<MappingView>> {
  return unwrapVersioned(api.POST('/api/v1/idp-group-mappings', { body: input }))
}

/**
 * The ETag of one mapping. A mapping is changed with `PUT` and `If-Match`, but the contract has
 * no `GET /idp-group-mappings/{id}` and the list carries no ETag, so there is no way to learn the
 * version of a mapping that was not just written. This reads it from the single object route that
 * the contract should offer (see the report); a server without it answers 404 or 405 and the
 * caller shows that error instead of overwriting blindly.
 */
export async function readMappingEtag(id: string): Promise<string> {
  const base = import.meta.env.VITE_API_BASE_URL ?? ''
  let response: Response
  try {
    response = await fetch(`${base}/api/v1/idp-group-mappings/${encodeURIComponent(id)}`, {
      credentials: 'include',
      headers: { Accept: 'application/json' },
    })
  } catch (cause) {
    throw networkError(cause)
  }
  if (!response.ok) {
    throw toApiError(response, await response.json().catch(() => null))
  }
  const etag = response.headers.get('etag')
  if (etag === null) {
    throw toApiError(new Response(null, { status: 428 }), null)
  }
  return etag
}

export async function updateMapping(id: string, input: MappingUpdate): Promise<MappingView> {
  const etag = await readMappingEtag(id)
  return (
    await unwrapVersioned(
      api.PUT('/api/v1/idp-group-mappings/{mapping_id}', {
        params: { path: { mapping_id: id }, header: { 'If-Match': etag } },
        body: input,
      }),
    )
  ).data
}

export function useDeleteMapping() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE('/api/v1/idp-group-mappings/{mapping_id}', {
          params: { path: { mapping_id: id } },
        }),
      ),
    // Role lists show how many groups map to a role.
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: mappingKeys.all }),
        queryClient.invalidateQueries({ queryKey: ['roles'] }),
      ]),
  })
}

export function testMappings(provider: Provider, groups: string[]): Promise<MappingTestResult> {
  return unwrap(api.POST('/api/v1/idp-group-mappings/test', { body: { provider, groups } }))
}
