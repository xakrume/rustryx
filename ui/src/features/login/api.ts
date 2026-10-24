import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'

/** Public facts (version, banner) for the footer of the sign in page. Anonymous callers may read them. */
export const systemInfoQuery = queryOptions({
  queryKey: ['system', 'info', 'public'] as const,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/info', { signal })),
  staleTime: STALE_TIME.static,
  // A failed footer must never slow the page or show an error: one quiet attempt.
  retry: false,
})
