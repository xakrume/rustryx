import { queryOptions } from '@tanstack/react-query'
import { api } from '@/api/client'
import { STALE_TIME } from '@/api/query-client'
import { unwrap, unwrapVersioned } from '@/api/request'
import type { components } from '@/api/schema'

export type SettingsSection = components['schemas']['SettingsSection']
export type SectionName = SettingsSection['section']

export const settingsKeys = {
  all: ['settings'] as const,
  schema: () => [...settingsKeys.all, 'schema'] as const,
  section: (name: string) => [...settingsKeys.all, 'section', name] as const,
}

export const settingsSchemaQuery = queryOptions({
  queryKey: settingsKeys.schema(),
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/settings/schema', { signal })),
  staleTime: STALE_TIME.static,
})

export function fetchSection(name: SectionName, signal?: AbortSignal) {
  return unwrapVersioned(
    api.GET('/api/v1/settings/{section}', { params: { path: { section: name } }, signal }),
  )
}

export const sectionQuery = (name: SectionName) =>
  queryOptions({
    queryKey: settingsKeys.section(name),
    queryFn: ({ signal }) => fetchSection(name, signal),
    staleTime: STALE_TIME.default,
  })

export function updateSection(
  name: SectionName,
  values: Record<string, unknown>,
  etag: string | null,
) {
  return unwrapVersioned(
    api.PUT('/api/v1/settings/{section}', {
      params: { path: { section: name }, header: { 'If-Match': etag ?? '' } },
      body: { values },
    }),
  )
}
