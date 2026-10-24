import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type SystemStatus = components['schemas']['SystemStatus']
export type StatusCheck = components['schemas']['StatusCheck']
export type NodeInfo = components['schemas']['NodeInfo']
export type SystemInfoDetails = components['schemas']['SystemInfoDetails']
export type MetricsSummary = components['schemas']['MetricsSummary']
export type SystemInfo = components['schemas']['SystemInfo']

export const systemKeys = {
  all: ['system'] as const,
  status: () => [...systemKeys.all, 'status'] as const,
  nodes: () => [...systemKeys.all, 'nodes'] as const,
  details: () => [...systemKeys.all, 'info-details'] as const,
  metrics: () => [...systemKeys.all, 'metrics'] as const,
  info: () => [...systemKeys.all, 'info'] as const,
}

/** Status is a live view: refreshed every 30 s while the tab is visible (paused in the background). */
export const STATUS_REFRESH_MS = 30_000

export const statusQuery = queryOptions({
  queryKey: systemKeys.status(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/status', { signal })),
  staleTime: STALE_TIME.live,
  refetchInterval: STATUS_REFRESH_MS,
})

export const nodesQuery = queryOptions({
  queryKey: systemKeys.nodes(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/nodes', { signal })),
  staleTime: STALE_TIME.live,
  refetchInterval: STATUS_REFRESH_MS,
})

export const infoDetailsQuery = queryOptions({
  queryKey: systemKeys.details(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/info/details', { signal })),
  staleTime: STALE_TIME.default,
})

export const metricsQuery = queryOptions({
  queryKey: systemKeys.metrics(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/metrics/summary', { signal })),
  staleTime: STALE_TIME.live,
  refetchInterval: STATUS_REFRESH_MS,
})

/** Public facts about the server (anonymous): the About page and anything that shows the version. */
export const systemInfoQuery = queryOptions({
  queryKey: systemKeys.info(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/info', { signal })),
  staleTime: STALE_TIME.static,
})
