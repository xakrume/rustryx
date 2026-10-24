import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'

export type RoutingRuleView = components['schemas']['RoutingRuleView']
export type RoutingRuleInput = components['schemas']['RoutingRuleInput']
export type RoutingValidateResult = components['schemas']['RoutingValidateResult']
export type RoutingTestResult = components['schemas']['RoutingTestResult']
export type RoutingMode = RoutingRuleInput['mode']

export interface RuleFilters {
  q: string | undefined
}

export const routingKeys = {
  all: ['routing-rules'] as const,
  list: (filters: RuleFilters) => [...routingKeys.all, 'list', filters] as const,
  detail: (id: string) => [...routingKeys.all, 'detail', id] as const,
}

export function fetchRulePage(
  filters: RuleFilters,
  after: string | null,
  limit: number,
  signal: AbortSignal,
) {
  return unwrap(
    api.GET('/api/v1/routing-rules', {
      params: { query: { q: filters.q, limit, after: after ?? undefined } },
      signal,
    }),
  )
}

export function fetchRule(id: string, signal?: AbortSignal) {
  return unwrapVersioned(
    api.GET('/api/v1/routing-rules/{rule_id}', { params: { path: { rule_id: id } }, signal }),
  )
}

export const ruleQuery = (id: string) =>
  queryOptions({
    queryKey: routingKeys.detail(id),
    queryFn: ({ signal }) => fetchRule(id, signal),
    staleTime: STALE_TIME.default,
  })

export function createRule(body: RoutingRuleInput): Promise<RoutingRuleView> {
  return unwrap(api.POST('/api/v1/routing-rules', { body }))
}

export function updateRule(id: string, body: RoutingRuleInput, etag: string | null) {
  return unwrapVersioned(
    api.PUT('/api/v1/routing-rules/{rule_id}', {
      params: { path: { rule_id: id }, header: { 'If-Match': etag ?? '' } },
      body,
    }),
  )
}

/** A routing rule in use has no `force`: the 409 lists the repositories (contract, decision 15). */
export function deleteRule(id: string, etag: string | null): Promise<void> {
  return unwrap(
    api.DELETE('/api/v1/routing-rules/{rule_id}', {
      params: {
        path: { rule_id: id },
        header: etag === null ? undefined : { 'If-Match': etag },
      },
    }),
  )
}

export function validatePatterns(patterns: string[], signal?: AbortSignal) {
  return unwrap(api.POST('/api/v1/routing-rules/validate', { body: { patterns }, signal }))
}

/** Tests an UNSAVED rule: the mode and patterns of the form, so a rule is understood before it runs. */
export function testDraftRule(mode: RoutingMode, patterns: string[], paths: string[]) {
  return unwrap(api.POST('/api/v1/routing-rules/test', { body: { mode, patterns, paths } }))
}
