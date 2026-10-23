import { useCallback, useState } from 'react'
import { DEFAULT_PAGE_SIZE, PAGE_SIZES, STORAGE_KEYS } from '@/lib/constants'
import { readJson, readStorage, writeJson, writeStorage } from '@/lib/storage'

type Visibility = Record<string, boolean>

const isVisibility = (value: unknown): value is Visibility =>
  typeof value === 'object' &&
  value !== null &&
  !Array.isArray(value) &&
  Object.values(value).every((v) => typeof v === 'boolean')

/**
 * Column visibility and page size are per viewer, per table, and live in localStorage. Stored
 * data is parsed defensively (a hand edited or foreign value yields the defaults) and writes
 * that fail are ignored: a preference is never worth an error.
 */
export function useTablePrefs(tableId: string) {
  const [visibility, setVisibilityState] = useState<Visibility>(() =>
    readJson(STORAGE_KEYS.tableColumns(tableId), isVisibility, {}),
  )
  const [pageSize, setPageSizeState] = useState<number>(() => {
    const stored = Number(readStorage(STORAGE_KEYS.tablePageSize(tableId)))
    return PAGE_SIZES.find((s) => s === stored) ?? DEFAULT_PAGE_SIZE
  })

  const setVisibility = useCallback(
    (next: Visibility) => {
      setVisibilityState(next)
      writeJson(STORAGE_KEYS.tableColumns(tableId), next)
    },
    [tableId],
  )
  const setPageSize = useCallback(
    (next: number) => {
      setPageSizeState(next)
      writeStorage(STORAGE_KEYS.tablePageSize(tableId), String(next))
    },
    [tableId],
  )
  return { visibility, setVisibility, pageSize, setPageSize }
}
