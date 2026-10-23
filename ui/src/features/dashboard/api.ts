import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type DashboardSummary = components['schemas']['DashboardSummary']

/**
 * The worked example of a feature's API module (docs/ui-architecture.md, "Adding a feature"):
 * a key factory, a query options object built from the typed client, `unwrap` for the error
 * type, the abort signal for cancellation, and a stale time picked from STALE_TIME.
 */
export const dashboardKeys = {
  all: ['dashboard'] as const,
  summary: () => [...dashboardKeys.all, 'summary'] as const,
}

export const dashboardQuery = queryOptions({
  queryKey: dashboardKeys.summary(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/dashboard/summary', { signal })),
  staleTime: STALE_TIME.default,
})
