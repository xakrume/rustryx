import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import type { InfinitePageConfig } from '@/api/pagination'
import { unwrap, unwrapVersioned, type Versioned } from '@/api/request'
import type { components } from '@/api/schema'

export type SelectorView = components['schemas']['ContentSelectorView']
export type SelectorInput = components['schemas']['ContentSelectorInput']
export type CompileIssue = components['schemas']['SelectorCompileIssue']
export type SelectorSample = components['schemas']['SelectorSample']
export type ValidateResult = components['schemas']['SelectorValidateResult']
export type TestResult = components['schemas']['SelectorTestResult']
export type PreviewResult = components['schemas']['SelectorPreviewResult']

export interface SelectorFilters {
  q?: string
}

export const selectorKeys = {
  all: ['selectors'] as const,
  list: (filters: SelectorFilters, limit: number) =>
    [...selectorKeys.all, 'list', filters, limit] as const,
  picker: () => [...selectorKeys.all, 'picker'] as const,
  detail: (id: string) => [...selectorKeys.all, 'detail', id] as const,
  language: () => [...selectorKeys.all, 'language'] as const,
}

export function selectorPage(
  filters: SelectorFilters,
  limit: number,
): InfinitePageConfig<SelectorView> {
  return {
    queryKey: selectorKeys.list(filters, limit),
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/content-selectors', {
          params: { query: { limit, after: after ?? undefined, q: filters.q } },
          signal,
        }),
      ),
  }
}

export const selectorPickerQuery = queryOptions({
  queryKey: selectorKeys.picker(),
  queryFn: async ({ signal }) =>
    (
      await unwrap(
        api.GET('/api/v1/content-selectors', { params: { query: { limit: 1000 } }, signal }),
      )
    ).items,
  staleTime: STALE_TIME.default,
})

export const selectorQuery = (id: string) =>
  queryOptions({
    queryKey: selectorKeys.detail(id),
    queryFn: ({ signal }): Promise<Versioned<SelectorView>> =>
      unwrapVersioned(
        api.GET('/api/v1/content-selectors/{selector_id}', {
          params: { path: { selector_id: id } },
          signal,
        }),
      ),
    staleTime: STALE_TIME.live,
  })

/** Help text of the expression language, produced by the engine so it cannot drift from it. */
export const selectorLanguageQuery = queryOptions({
  queryKey: selectorKeys.language(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/content-selectors/language', { signal })),
  staleTime: STALE_TIME.static,
})

export function createSelector(input: SelectorInput): Promise<Versioned<SelectorView>> {
  return unwrapVersioned(api.POST('/api/v1/content-selectors', { body: input }))
}

export function updateSelector(
  id: string,
  input: SelectorInput,
  etag: string | null,
): Promise<Versioned<SelectorView>> {
  return unwrapVersioned(
    api.PUT('/api/v1/content-selectors/{selector_id}', {
      params: { path: { selector_id: id }, header: { 'If-Match': etag ?? '' } },
      body: input,
    }),
  )
}

export function validateExpression(
  expression: string,
  signal?: AbortSignal,
): Promise<ValidateResult> {
  return unwrap(api.POST('/api/v1/content-selectors/validate', { body: { expression }, signal }))
}

export function testExpression(expression: string, samples: SelectorSample[]): Promise<TestResult> {
  return unwrap(api.POST('/api/v1/content-selectors/test', { body: { expression, samples } }))
}

export function previewExpression(expression: string, repository: string): Promise<PreviewResult> {
  return unwrap(
    api.POST('/api/v1/content-selectors/preview', { body: { expression, repository, limit: 10 } }),
  )
}

export function useDeleteSelector() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE('/api/v1/content-selectors/{selector_id}', {
          params: { path: { selector_id: id } },
        }),
      ),
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: selectorKeys.all }),
        queryClient.invalidateQueries({ queryKey: ['privileges'] }),
      ]),
  })
}
