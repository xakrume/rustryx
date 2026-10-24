import { useNavigate, useSearch } from '@tanstack/react-router'
import { useCallback } from 'react'

/**
 * `validateSearch` helper for the administration pages: a text parameter. The router parses
 * `?q=123` into the NUMBER 123 and `?view=true` into a boolean, so a text filter or a view name
 * would silently break; this turns them back into strings and drops everything else.
 */
export function text(value: unknown): string | undefined {
  if (typeof value === 'string' && value !== '') return value
  if (typeof value === 'number' && Number.isFinite(value)) return String(value)
  if (typeof value === 'boolean') return String(value)
  return undefined
}

export type SearchPatch = Readonly<Record<string, string | undefined>>

/** One text parameter of the address bar, or undefined when it is absent. */
export function useSearchParam(name: string): string | undefined {
  const search: Record<string, unknown> = useSearch({ strict: false })
  return text(search[name])
}

/**
 * Change address bar parameters of the CURRENT page. The administration pages keep their
 * secondary views (an editor, a task detail, a report) in the address bar instead of in extra
 * routes: the route tree stays fixed, a view can be linked and bookmarked, the back button
 * closes it, and the unsaved-changes guard sees the navigation. `replace` is for edits that
 * should not fill the history (typing in a filter); opening a view pushes an entry.
 */
export function useSearchPatch(): (patch: SearchPatch, options?: { replace?: boolean }) => void {
  const navigate = useNavigate()
  return useCallback(
    (patch, options) => {
      void navigate({
        to: '.',
        replace: options?.replace ?? false,
        search: ((previous: Record<string, unknown>) => ({ ...previous, ...patch })) as never,
      })
    },
    [navigate],
  )
}
