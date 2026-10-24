import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'

export type BlobStoreView = components['schemas']['BlobStoreView']
export type BlobStoreKind = components['schemas']['BlobStoreKind']
export type BlobStoreUsage = components['schemas']['BlobStoreUsage']
export type BlobStoreCreate = components['schemas']['BlobStoreCreate']
export type BlobStorePatch = components['schemas']['BlobStorePatch']
export type BlobStoreTestResult = components['schemas']['BlobStoreTestResult']
export type RepositoryView = components['schemas']['RepositoryView']

export interface BlobStoreFilters {
  q: string | undefined
  kind: string | undefined
}

export const blobStoreKeys = {
  all: ['blob-stores'] as const,
  list: (filters: BlobStoreFilters) => [...blobStoreKeys.all, 'list', filters] as const,
  detail: (name: string) => [...blobStoreKeys.all, 'detail', name] as const,
  usage: (name: string) => [...blobStoreKeys.all, 'usage', name] as const,
  repositories: (name: string) => [...blobStoreKeys.all, 'repositories', name] as const,
}

export function fetchBlobStorePage(
  filters: BlobStoreFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/blob-stores', {
      params: {
        query: {
          q: filters.q,
          kind: filters.kind === 'local' || filters.kind === 's3' ? filters.kind : undefined,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

/** The object and its ETag: the version being edited goes back in `If-Match`. */
export function fetchBlobStore(name: string, signal?: AbortSignal) {
  return unwrapVersioned(
    api.GET('/api/v1/blob-stores/{blob_store_name}', {
      params: { path: { blob_store_name: name } },
      signal,
    }),
  )
}

export const blobStoreQuery = (name: string) =>
  queryOptions({
    queryKey: blobStoreKeys.detail(name),
    queryFn: ({ signal }) => fetchBlobStore(name, signal),
    staleTime: STALE_TIME.default,
  })

/** Fresh numbers (a few queries on the server): the detail page asks, the list does not. */
export const blobStoreUsageQuery = (name: string) =>
  queryOptions({
    queryKey: blobStoreKeys.usage(name),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/blob-stores/{blob_store_name}/usage', {
          params: { path: { blob_store_name: name } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.live,
  })

/** The first repositories that use the store; the contract has no count-by-store beyond `repository_count`. */
export const REPOSITORY_PREVIEW_LIMIT = 20

export const blobStoreRepositoriesQuery = (name: string) =>
  queryOptions({
    queryKey: blobStoreKeys.repositories(name),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/repositories', {
          params: { query: { blob_store: name, limit: REPOSITORY_PREVIEW_LIMIT } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.default,
  })

export function createBlobStore(body: BlobStoreCreate): Promise<BlobStoreView> {
  return unwrap(api.POST('/api/v1/blob-stores', { body }))
}

export function patchBlobStore(name: string, patch: BlobStorePatch, etag: string | null) {
  return unwrapVersioned(
    api.PATCH('/api/v1/blob-stores/{blob_store_name}', {
      params: { path: { blob_store_name: name }, header: { 'If-Match': etag ?? '' } },
      // The contract's body type is a merge patch (RFC 7396): `null` clears, objects merge.
      headers: { 'Content-Type': 'application/merge-patch+json' },
      body: patch,
    }),
  )
}

export function deleteBlobStore(name: string, etag: string | null): Promise<void> {
  return unwrap(
    api.DELETE('/api/v1/blob-stores/{blob_store_name}', {
      params: {
        path: { blob_store_name: name },
        header: etag === null ? undefined : { 'If-Match': etag },
      },
    }),
  )
}

export function testBlobStoreConnection(name: string): Promise<BlobStoreTestResult> {
  return unwrap(
    api.POST('/api/v1/blob-stores/{blob_store_name}/actions/test-connection', {
      params: { path: { blob_store_name: name } },
    }),
  )
}
