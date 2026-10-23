import { useNavigate, useSearch } from '@tanstack/react-router'
import { useCallback, useMemo } from 'react'

export interface UrlTableState {
  /** Current value of each filter id; absent means no filter. */
  filters: Readonly<Record<string, string | undefined>>
  /** The API sort value ("name", "-name") or undefined for the endpoint default. */
  sort: string | undefined
  setFilter: (id: string, value: string | null) => void
  clearFilters: () => void
  setSort: (sort: string) => void
  hasActiveFilters: boolean
}

/**
 * Filters and sort of a list live in the address bar (docs/ui-page-inventory.md, principle 2),
 * so a view can be bookmarked, shared and survives a reload and the back button.
 *
 * The route MUST validate its search with a schema that lists these keys as optional strings
 * (`validateSearch: z.object({ q: z.string().optional(), sort: z.string().optional() })`):
 * without it the router parses `?q=123` into the NUMBER 123 and a text filter breaks.
 * Edits replace the history entry, so typing in a filter does not fill the back button.
 * A changed filter or sort starts a new query (it is part of the query key), and the old
 * keyset cursor is never reused, because the page token is bound to filters and sort.
 */
export function useUrlTableState(filterIds: readonly string[]): UrlTableState {
  const search: Record<string, unknown> = useSearch({ strict: false })
  const navigate = useNavigate()

  const filters = useMemo(() => {
    const out: Record<string, string | undefined> = {}
    for (const id of filterIds) {
      const value = search[id]
      out[id] = typeof value === 'string' && value !== '' ? value : undefined
    }
    return out
  }, [search, filterIds])

  const sort = typeof search.sort === 'string' && search.sort !== '' ? search.sort : undefined

  const patch = useCallback(
    (changes: Record<string, string | undefined>) => {
      void navigate({
        to: '.',
        replace: true,
        search: ((previous: Record<string, unknown>) => ({ ...previous, ...changes })) as never,
      })
    },
    [navigate],
  )

  const setFilter = useCallback(
    (id: string, value: string | null) =>
      patch({ [id]: value === null || value === '' ? undefined : value }),
    [patch],
  )
  const clearFilters = useCallback(
    () => patch(Object.fromEntries(filterIds.map((id) => [id, undefined]))),
    [patch, filterIds],
  )
  const setSort = useCallback((next: string) => patch({ sort: next }), [patch])

  return {
    filters,
    sort,
    setFilter,
    clearFilters,
    setSort,
    hasActiveFilters: filterIds.some((id) => filters[id] !== undefined),
  }
}
