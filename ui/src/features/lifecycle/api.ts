import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { toApiError } from '@/api/errors'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'
import { isActiveRun, pollInterval } from '@/features/system/run-status'

export type LifecycleRun = components['schemas']['LifecycleRun']
export type LifecycleCandidate = components['schemas']['LifecycleCandidate']
export type GcRun = components['schemas']['GcRun']
export type GcCandidate = components['schemas']['GcCandidate']

export type RunSource = 'cleanup' | 'gc'

export interface RunFilters {
  /** `dry` or `real`; anything else means both. */
  mode: string | undefined
  status: string | undefined
  policyId?: string | undefined
}

export interface CandidateFilters {
  repository: string | undefined
  /** `yes` or `no`; anything else means both. */
  protected: string | undefined
}

export const lifecycleKeys = {
  all: ['lifecycle'] as const,
  runs: (source: RunSource, filters: RunFilters) =>
    [...lifecycleKeys.all, 'runs', source, filters] as const,
  run: (source: RunSource, id: string) => [...lifecycleKeys.all, 'run', source, id] as const,
  candidates: (source: RunSource, id: string, filters: CandidateFilters) =>
    [...lifecycleKeys.all, 'candidates', source, id, filters] as const,
}

const dryRunParam = (mode: string | undefined): boolean | undefined =>
  mode === 'dry' ? true : mode === 'real' ? false : undefined

const flag = (value: string | undefined): boolean | undefined =>
  value === 'yes' ? true : value === 'no' ? false : undefined

export function fetchLifecycleRunPage(
  filters: RunFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/lifecycle-runs', {
      params: {
        query: {
          dry_run: dryRunParam(filters.mode),
          status: filters.status as LifecycleRun['status'] | undefined,
          policy_id: filters.policyId,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

export function fetchGcRunPage(
  filters: RunFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/gc/runs', {
      params: {
        query: {
          dry_run: dryRunParam(filters.mode),
          status: filters.status as GcRun['status'] | undefined,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

/**
 * A run that is still queued or running is polled (2 s, then 5 s, then 15 s) until it ends;
 * the polling pauses while the tab is hidden, which is TanStack Query's default.
 */
export const lifecycleRunQuery = (id: string) =>
  queryOptions({
    queryKey: lifecycleKeys.run('cleanup', id),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/lifecycle-runs/{run_id}', { params: { path: { run_id: id } }, signal }),
      ),
    staleTime: STALE_TIME.live,
    refetchInterval: (query) =>
      isActiveRun(query.state.data?.status) ? pollInterval(query.state.dataUpdateCount) : false,
  })

export const gcRunQuery = (id: string) =>
  queryOptions({
    queryKey: lifecycleKeys.run('gc', id),
    queryFn: ({ signal }) =>
      unwrap(api.GET('/api/v1/gc/runs/{run_id}', { params: { path: { run_id: id } }, signal })),
    staleTime: STALE_TIME.live,
    refetchInterval: (query) =>
      isActiveRun(query.state.data?.status) ? pollInterval(query.state.dataUpdateCount) : false,
  })

export async function fetchCandidatePage(
  runId: string,
  filters: CandidateFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
): Promise<components['schemas']['LifecycleCandidatePage']> {
  // The operation can answer JSON or CSV, so the typed body is a union: this call asks for JSON.
  const body = await unwrap(
    api.GET('/api/v1/lifecycle-runs/{run_id}/candidates', {
      params: {
        path: { run_id: runId },
        query: {
          repository: filters.repository,
          protected: flag(filters.protected),
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
  return body as components['schemas']['LifecycleCandidatePage']
}

export function fetchGcCandidatePage(
  runId: string,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/gc/runs/{run_id}/candidates', {
      params: { path: { run_id: runId }, query: { limit, after: after ?? undefined } },
      signal,
    }),
  )
}

/** Start a dry run of a SAVED cleanup policy: 202 with the queued run (long running, section 14). */
export function startCleanupPreview(policyId: string): Promise<LifecycleRun> {
  return unwrap(
    api.POST('/api/v1/cleanup-policies/{policy_id}/actions/preview', {
      params: { path: { policy_id: policyId } },
    }),
  )
}

/**
 * The whole plan as CSV (`Accept: text/csv`: the server streams every row and ignores paging).
 * Read as text and handed to the browser as a file; nothing is kept.
 */
export async function fetchCandidatesCsv(runId: string): Promise<string> {
  const { data, error, response } = await api.GET('/api/v1/lifecycle-runs/{run_id}/candidates', {
    params: { path: { run_id: runId } },
    headers: { Accept: 'text/csv' },
    parseAs: 'text',
  })
  if (!response.ok) throw toApiError(response, error)
  return typeof data === 'string' ? data : ''
}
