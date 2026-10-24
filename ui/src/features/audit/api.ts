import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import type { InfinitePageConfig } from '@/api/pagination'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components, paths } from '@/api/schema'

export type AuditEvent = components['schemas']['AuditEvent']
export type AuditKind = components['schemas']['AuditKind']

export const AUDIT_KINDS: readonly AuditKind[] = [
  'login_success',
  'login_failure',
  'rbac_change',
  'token_change',
  'config_change',
  'content_change',
]

export type RangeKey = 'hour' | 'day' | 'week' | 'month' | 'custom'

/** What the address bar holds: every value is text, as the router hands it over. */
export interface AuditFilters {
  /** Comma separated kinds. */
  kind?: string
  actor?: string
  action?: string
  target?: string
  repository?: string
  project?: string
  request_id?: string
  outcome?: string
  range?: string
  from?: string
  to?: string
}

export interface TimeBounds {
  from?: string
  to?: string
}

const SPAN_MS: Record<Exclude<RangeKey, 'custom'>, number> = {
  hour: 3_600_000,
  day: 86_400_000,
  week: 7 * 86_400_000,
  month: 30 * 86_400_000,
}

const iso = (ms: number): string => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')

/**
 * The instants a time filter means. A relative range is anchored to `now` ONCE, when the filter
 * is set or refreshed, never on every request: the page token of a keyset page is bound to the
 * filters, so a window that slid between two pages would make the next page invalid.
 */
export function resolveBounds(filters: AuditFilters, now: number): TimeBounds {
  const range = filters.range
  if (range === 'hour' || range === 'day' || range === 'week' || range === 'month') {
    return { from: iso(now - SPAN_MS[range]) }
  }
  if (range === 'custom') return { from: filters.from, to: filters.to }
  return {}
}

export const auditKeys = {
  all: ['audit'] as const,
  list: (filters: AuditFilters, bounds: TimeBounds, sort: string, limit: number) =>
    [...auditKeys.all, 'list', filters, bounds, sort, limit] as const,
  event: (id: string) => [...auditKeys.all, 'event', id] as const,
}

type Query = NonNullable<paths['/api/v1/audit-events']['get']['parameters']['query']>

/** The query parameters of the list and the export, which take the same filters. */
export function auditQuery(filters: AuditFilters, bounds: TimeBounds): Query {
  const kinds = (filters.kind ?? '')
    .split(',')
    .filter((k): k is AuditKind => AUDIT_KINDS.includes(k as AuditKind))
  return {
    kind: kinds.length > 0 ? kinds : undefined,
    actor: filters.actor,
    action: filters.action,
    target: filters.target,
    repository: filters.repository,
    project: filters.project,
    request_id: filters.request_id,
    outcome:
      filters.outcome === 'success' || filters.outcome === 'failure' ? filters.outcome : undefined,
    from: bounds.from,
    to: bounds.to,
  }
}

export function auditPage(
  filters: AuditFilters,
  bounds: TimeBounds,
  sort: '-at' | 'at',
  limit: number,
): InfinitePageConfig<AuditEvent> {
  return {
    queryKey: auditKeys.list(filters, bounds, sort, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/audit-events', {
          params: {
            query: { ...auditQuery(filters, bounds), sort, limit, after: after ?? undefined },
          },
          signal,
        }),
      ),
  }
}

export const auditEventQuery = (id: string) =>
  queryOptions({
    queryKey: auditKeys.event(id),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/audit-events/{event_id}', { params: { path: { event_id: id } }, signal }),
      ),
    staleTime: STALE_TIME.default,
  })
