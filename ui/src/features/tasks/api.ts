import { infiniteQueryOptions, queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'
import { isActiveRun, pollInterval } from '@/features/system/run-status'

export type Task = components['schemas']['Task']
export type TaskRun = components['schemas']['TaskRun']
export type TaskLogPage = components['schemas']['TaskLogPage']

export interface TaskFilters {
  kind: string | undefined
  /** `system` or `user`; anything else means both. */
  scope: string | undefined
  /** `on` or `off`; anything else means both. */
  enabled: string | undefined
}

export const taskKeys = {
  all: ['tasks'] as const,
  list: (filters: TaskFilters) => [...taskKeys.all, 'list', filters] as const,
  detail: (id: string) => [...taskKeys.all, 'detail', id] as const,
  runs: (id: string, status: string | undefined) => [...taskKeys.all, 'runs', id, status] as const,
  run: (id: string, run: string) => [...taskKeys.all, 'run', id, run] as const,
  log: (id: string, run: string) => [...taskKeys.all, 'log', id, run] as const,
}

export function fetchTaskPage(
  filters: TaskFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/tasks', {
      params: {
        query: {
          kind: filters.kind,
          system: filters.scope === 'system' ? true : filters.scope === 'user' ? false : undefined,
          enabled: filters.enabled === 'on' ? true : filters.enabled === 'off' ? false : undefined,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

export const taskQuery = (id: string) =>
  queryOptions({
    queryKey: taskKeys.detail(id),
    queryFn: ({ signal }) =>
      unwrap(api.GET('/api/v1/tasks/{task_id}', { params: { path: { task_id: id } }, signal })),
    staleTime: STALE_TIME.live,
  })

export function fetchTaskRunPage(
  taskId: string,
  status: string | undefined,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/tasks/{task_id}/runs', {
      params: {
        path: { task_id: taskId },
        query: {
          status: status as TaskRun['status'] | undefined,
          limit,
          after: after ?? undefined,
        },
      },
      signal,
    }),
  )
}

/** A run that is queued or running is polled (2 s, 5 s, 15 s) until it ends. */
export const taskRunQuery = (taskId: string, runId: string) =>
  queryOptions({
    queryKey: taskKeys.run(taskId, runId),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/tasks/{task_id}/runs/{run_id}', {
          params: { path: { task_id: taskId, run_id: runId } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.live,
    refetchInterval: (query) =>
      isActiveRun(query.state.data?.status) ? pollInterval(query.state.dataUpdateCount) : false,
  })

/** Lines per window. The log is read forward by line number (`from_line`), never as one blob. */
export const LOG_WINDOW = 500

export function fetchLogWindow(
  taskId: string,
  runId: string,
  fromLine: number,
  signal: AbortSignal,
): Promise<TaskLogPage> {
  return unwrap(
    api.GET('/api/v1/tasks/{task_id}/runs/{run_id}/log', {
      params: {
        path: { task_id: taskId, run_id: runId },
        query: { from_line: fromLine, limit: LOG_WINDOW },
      },
      signal,
    }),
  )
}

export const taskLogQuery = (taskId: string, runId: string) =>
  infiniteQueryOptions({
    queryKey: taskKeys.log(taskId, runId),
    initialPageParam: 0,
    queryFn: ({ pageParam, signal }) => fetchLogWindow(taskId, runId, pageParam, signal),
    // More to read when the window was full or the run is still writing: the cursor is the
    // server's `next_line`, or the line after the last one received.
    getNextPageParam: (last, _all, lastParam) => {
      const lastLine = last.lines.at(-1)?.n
      const cursor = last.next_line ?? (lastLine === undefined ? lastParam : lastLine + 1)
      return last.finished && last.lines.length < LOG_WINDOW ? undefined : cursor
    },
    staleTime: STALE_TIME.live,
  })
