import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'
import type { CleanupPolicyInput, CleanupPolicyView } from './criteria'

export type FormatInfo = components['schemas']['FormatInfo']

export interface PolicyFilters {
  q: string | undefined
  format: string | undefined
}

export const cleanupKeys = {
  all: ['cleanup-policies'] as const,
  list: (filters: PolicyFilters) => [...cleanupKeys.all, 'list', filters] as const,
  detail: (id: string) => [...cleanupKeys.all, 'detail', id] as const,
  formats: () => [...cleanupKeys.all, 'formats'] as const,
}

export function fetchPolicyPage(
  filters: PolicyFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/cleanup-policies', {
      params: {
        query: {
          q: filters.q,
          format: filters.format as FormatInfo['format'] | undefined,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

export function fetchPolicy(id: string, signal?: AbortSignal) {
  return unwrapVersioned(
    api.GET('/api/v1/cleanup-policies/{policy_id}', {
      params: { path: { policy_id: id } },
      signal,
    }),
  )
}

export const policyQuery = (id: string) =>
  queryOptions({
    queryKey: cleanupKeys.detail(id),
    queryFn: ({ signal }) => fetchPolicy(id, signal),
    staleTime: STALE_TIME.default,
  })

/** Which criteria each format supports comes from the server: the formats declare them. */
export const formatsQuery = queryOptions({
  queryKey: cleanupKeys.formats(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/formats', { signal })),
  staleTime: STALE_TIME.static,
})

export function createPolicy(body: CleanupPolicyInput): Promise<CleanupPolicyView> {
  return unwrap(api.POST('/api/v1/cleanup-policies', { body }))
}

export function updatePolicy(id: string, body: CleanupPolicyInput, etag: string | null) {
  return unwrapVersioned(
    api.PUT('/api/v1/cleanup-policies/{policy_id}', {
      params: { path: { policy_id: id }, header: { 'If-Match': etag ?? '' } },
      body,
    }),
  )
}

/** `force` detaches the policy from its repositories first (contract: delete in use is 409 without it). */
export function deletePolicy(id: string, options: { force: boolean; etag: string | null }) {
  return unwrap(
    api.DELETE('/api/v1/cleanup-policies/{policy_id}', {
      params: {
        path: { policy_id: id },
        query: { force: options.force },
        header: options.etag === null ? undefined : { 'If-Match': options.etag },
      },
    }),
  )
}
