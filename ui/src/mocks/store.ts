import { HttpResponse, type JsonBodyType } from 'msw'
import type { components } from '@/api/schema'
import { buildRepositories } from './data'
import { readSession, userOf } from './session'

/**
 * Shared state and helpers of the mock handlers, importable by the feature mock modules
 * (src/features/<name>/mocks.ts) without importing the handler list itself.
 */
type RepositoryView = components['schemas']['RepositoryView']

/** A little latency in the browser mock makes loading states visible; tests stay instant. */
export const LATENCY_MS = typeof window !== 'undefined' && __API_MOCK__ ? 120 : 0

export const repositories = buildRepositories()

/** Activity is always "just happened" in the mock: a fixed instant would drift into the past or future. */
export const minutesAgo = (minutes: number): string =>
  new Date(Date.now() - minutes * 60_000).toISOString().replace(/\.\d{3}Z$/, 'Z')

export const json = (body: JsonBodyType, init?: ResponseInit): Response =>
  HttpResponse.json(body, init)

/** Anonymous callers see only repositories that are public (both anonymous switches on). */
export function visibleRepositories(): RepositoryView[] {
  return userOf(readSession()) === null
    ? repositories.filter((r) => r.allow_anonymous)
    : repositories
}
