import { useNavigate, useSearch } from '@tanstack/react-router'
import { useCallback } from 'react'

/**
 * Keep the text values of a route's search object and drop everything else. The router parses
 * `?q=123` into the NUMBER 123, which would break a text filter, so numbers and booleans are
 * turned back into strings; objects, arrays and empty values are dropped, never an error page.
 * Hand written instead of zod on purpose: `validateSearch` is not code split, and zod would end up
 * in the entry bundle (docs/ui-architecture.md, section 13).
 */
export function pickText<K extends string>(
  search: Record<string, unknown>,
  keys: readonly K[],
): Partial<Record<K, string>> {
  const out: Partial<Record<K, string>> = {}
  for (const key of keys) {
    const value = search[key]
    if (typeof value === 'string' && value !== '') out[key] = value
    else if (typeof value === 'number' || typeof value === 'boolean') out[key] = String(value)
  }
  return out
}

/**
 * One search parameter of the current route, for state that belongs in the address bar but is not
 * a list filter: which editor dialog is open (`?edit=<id>`, `?new=1`), which detail drawer.
 * Setting it adds a history entry, so the Back button closes the dialog the way users expect.
 */
export function useSearchParam(
  key: string,
): readonly [string | undefined, (next: string | undefined) => void] {
  const search: Record<string, unknown> = useSearch({ strict: false })
  const navigate = useNavigate()
  const raw = search[key]
  const value =
    typeof raw === 'string' && raw !== '' ? raw : typeof raw === 'number' ? String(raw) : undefined
  const set = useCallback(
    (next: string | undefined) => {
      void navigate({
        to: '.',
        search: ((previous: Record<string, unknown>) => ({ ...previous, [key]: next })) as never,
      })
    },
    [navigate, key],
  )
  return [value, set]
}

/** Remove several search parameters in ONE navigation (closing an editor must not stack entries). */
export function useClearSearchParams(keys: readonly string[]): () => void {
  const navigate = useNavigate()
  return useCallback(() => {
    void navigate({
      to: '.',
      search: ((previous: Record<string, unknown>) => ({
        ...previous,
        ...Object.fromEntries(keys.map((key) => [key, undefined])),
      })) as never,
    })
  }, [navigate, keys])
}
