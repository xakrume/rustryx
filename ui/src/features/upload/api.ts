import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

/** The upload form of a format is built by the format crate; it never changes during a session. */
export const uploadSchemaQuery = (format: components['schemas']['RepoFormat']) =>
  queryOptions({
    queryKey: ['upload-schema', format] as const,
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/formats/{format}/upload-schema', { params: { path: { format } }, signal }),
      ),
    staleTime: STALE_TIME.static,
  })
