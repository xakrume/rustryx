import {
  flexRender,
  getCoreRowModel,
  useReactTable,
  type ColumnDef,
  type RowSelectionState,
  type VisibilityState,
} from '@tanstack/react-table'
import { ArrowDown, ArrowUp, ArrowUpDown, Inbox } from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { DensityToggle } from '@/components/density-toggle'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Select } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { t } from '@/i18n'
import { PAGE_SIZES } from '@/lib/constants'
import { cn } from '@/lib/utils'
import { ColumnMenu } from './column-menu'
import './types'
import { useTablePrefs } from './use-table-prefs'

/**
 * Columns of any value type. TanStack Table types a column by its value, so a list of mixed
 * columns (string, number, date) only fits `any` there; this alias is the one place that says so.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any -- TanStack Table column value types differ per column
export type AnyColumnDef<T> = ColumnDef<T, any>

export interface BulkContext<T> {
  selectedRows: T[]
  clearSelection: () => void
}

export interface DataTableProps<T> {
  /** Stable id. Column visibility and page size are remembered per tableId in this browser. */
  tableId: string
  /** Accessible name of the table and its scroll region: "Repositories". */
  label: string
  columns: AnyColumnDef<T>[]
  rows: readonly T[]
  getRowId: (row: T) => string

  // Query state, as returned by useInfinitePage.
  isLoading: boolean
  error?: unknown
  onRetry?: () => void
  hasNextPage?: boolean
  isLoadingMore?: boolean
  onLoadMore?: () => void

  /** Current API sort value: "name" or "-name". Sorting happens on the server. */
  sort?: string
  onSortChange?: (sort: string) => void

  /** Rendered above the table: usually a FilterBar. */
  toolbar?: ReactNode
  /** Distinguishes "nothing exists yet" from "nothing matches these filters". */
  hasActiveFilters?: boolean
  onClearFilters?: () => void
  /** First use empty state. Default: a generic message. */
  emptyState?: ReactNode
  /** Page size is sent to the API as `limit`. The parent owns the value (see useTablePrefs). */
  pageSize?: number
  onPageSizeChange?: (size: number) => void

  selectable?: boolean
  /** Label of one row's checkbox: "Select maven-releases". */
  selectionLabel?: (row: T) => string
  /** Content of the bulk action bar. It appears while at least one row is selected. */
  bulkActions?: (context: BulkContext<T>) => ReactNode
  /** Change it when filters or sort change: it clears the selection, so a bulk action never hits unseen rows. */
  resetSelectionKey?: string

  className?: string
}

/** Skeleton rows show only after this long, so a fast answer never flashes one (inventory, 5). */
const SKELETON_DELAY_MS = 150
const SKELETON_ROWS = 8

/**
 * The table every list page uses. Data, filters, sort and paging are server side (keyset
 * pagination, "load more"); this component only presents them: sortable headers with
 * aria-sort, sticky header, column chooser, row selection with a bulk action bar, density,
 * delayed loading skeleton, empty / filtered / error states, and a keyboard focusable
 * horizontal scroll region for narrow screens.
 *
 * Stale rows stay on screen while a refresh or a "load more" fails: the error shows above
 * them with a Retry, nothing the user could still read disappears.
 */
export function DataTable<T>({
  tableId,
  label,
  columns,
  rows,
  getRowId,
  isLoading,
  error,
  onRetry,
  hasNextPage = false,
  isLoadingMore = false,
  onLoadMore,
  sort,
  onSortChange,
  toolbar,
  hasActiveFilters = false,
  onClearFilters,
  emptyState,
  pageSize,
  onPageSizeChange,
  selectable = false,
  selectionLabel,
  bulkActions,
  resetSelectionKey,
  className,
}: DataTableProps<T>) {
  const prefs = useTablePrefs(tableId)
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({})
  const showSkeleton = useDelayedFlag(isLoading, SKELETON_DELAY_MS)

  useEffect(() => {
    setRowSelection({})
  }, [resetSelectionKey])

  const allColumns = useMemo<AnyColumnDef<T>[]>(() => {
    if (!selectable) return columns
    const selectColumn: AnyColumnDef<T> = {
      id: '__select',
      enableHiding: false,
      meta: { label: t('table.selectRow'), hideable: false, className: 'w-10' },
      header: ({ table }) => (
        <Checkbox
          aria-label={t('table.selectAll')}
          checked={
            table.getIsAllRowsSelected()
              ? true
              : table.getIsSomeRowsSelected()
                ? 'indeterminate'
                : false
          }
          onCheckedChange={(checked) => table.toggleAllRowsSelected(checked === true)}
        />
      ),
      cell: ({ row }) => (
        <Checkbox
          aria-label={selectionLabel ? selectionLabel(row.original) : t('table.selectRow')}
          checked={row.getIsSelected()}
          onCheckedChange={(checked) => row.toggleSelected(checked === true)}
        />
      ),
    }
    return [selectColumn, ...columns]
  }, [columns, selectable, selectionLabel])

  const columnVisibility: VisibilityState = prefs.visibility
  const table = useReactTable<T>({
    data: rows as T[],
    columns: allColumns,
    getRowId,
    getCoreRowModel: getCoreRowModel(),
    manualSorting: true,
    manualFiltering: true,
    manualPagination: true,
    enableRowSelection: selectable,
    state: { rowSelection, columnVisibility },
    onRowSelectionChange: setRowSelection,
    onColumnVisibilityChange: (updater) =>
      prefs.setVisibility(typeof updater === 'function' ? updater(columnVisibility) : updater),
  })

  const selectedRows = table.getSelectedRowModel().rows.map((r) => r.original)
  const visibleColumnCount = table.getVisibleLeafColumns().length
  const hasRows = rows.length > 0
  const firstLoad = isLoading && !hasRows

  function toggleSort(sortKey: string): void {
    onSortChange?.(sort === sortKey ? `-${sortKey}` : sortKey)
  }

  function ariaSort(sortKey: string | undefined): 'ascending' | 'descending' | 'none' | undefined {
    if (sortKey === undefined) return undefined
    if (sort === sortKey) return 'ascending'
    if (sort === `-${sortKey}`) return 'descending'
    return 'none'
  }

  let body: ReactNode
  if (firstLoad) {
    body = showSkeleton
      ? Array.from({ length: SKELETON_ROWS }, (_, i) => (
          <TableRow key={`skeleton-${i}`}>
            {Array.from({ length: visibleColumnCount }, (_, c) => (
              <TableCell key={c}>
                <Skeleton className="h-4 w-full max-w-48" />
              </TableCell>
            ))}
          </TableRow>
        ))
      : null
  } else {
    body = table.getRowModel().rows.map((row) => (
      <TableRow key={row.id} data-state={row.getIsSelected() ? 'selected' : undefined}>
        {row.getVisibleCells().map((cell) => (
          <TableCell
            key={cell.id}
            className={cn(
              // One line per cell: a narrow screen scrolls the table sideways instead of breaking
              // names such as maven-releases at their hyphens. A column may opt out via meta.className.
              'whitespace-nowrap',
              cell.column.columnDef.meta?.align === 'right' && 'text-right tabular-nums',
              cell.column.columnDef.meta?.className,
            )}
          >
            {flexRender(cell.column.columnDef.cell, cell.getContext())}
          </TableCell>
        ))}
      </TableRow>
    ))
  }

  const showEmpty = !isLoading && !error && !hasRows
  const showFatalError = error !== undefined && error !== null && !hasRows && !isLoading
  const showInlineError = error !== undefined && error !== null && hasRows

  return (
    <div className={cn('space-y-3', className)}>
      {toolbar}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-h-8 items-center gap-2">
          {selectable && selectedRows.length > 0 ? (
            <div
              role="region"
              aria-label={t('table.bulkActions')}
              className="flex flex-wrap items-center gap-2 rounded-md border border-info bg-info-subtle px-2 py-1 text-sm text-info-text"
            >
              <span aria-live="polite" className="font-medium">
                {t('table.selected', { count: selectedRows.length })}
              </span>
              {bulkActions?.({ selectedRows, clearSelection: () => setRowSelection({}) })}
              <Button variant="ghost" size="sm" onClick={() => setRowSelection({})}>
                {t('table.clearSelection')}
              </Button>
            </div>
          ) : null}
        </div>
        <div className="flex items-center gap-1">
          <ColumnMenu table={table} />
          <DensityToggle />
        </div>
      </div>

      {showFatalError ? (
        <ErrorPanel error={error} onRetry={onRetry} title={t('table.errorTitle')} />
      ) : showEmpty ? (
        hasActiveFilters ? (
          <EmptyState
            icon={Inbox}
            title={t('table.emptyFiltered')}
            description={t('table.emptyFilteredHint')}
            action={
              onClearFilters ? (
                <Button variant="outline" onClick={onClearFilters}>
                  {t('table.clearFilters')}
                </Button>
              ) : undefined
            }
          />
        ) : (
          (emptyState ?? <EmptyState icon={Inbox} title={t('table.emptyFirstUse')} />)
        )
      ) : (
        <>
          {showInlineError ? (
            <ErrorPanel error={error} onRetry={onRetry} title={t('table.stale')} />
          ) : null}
          <Table
            label={label}
            aria-busy={isLoading}
            containerClassName={cn('max-h-[70dvh]', firstLoad && 'min-h-48')}
          >
            <TableCaption className="sr-only">{label}</TableCaption>
            <TableHeader>
              {table.getHeaderGroups().map((group) => (
                <TableRow key={group.id} className="hover:bg-muted">
                  {group.headers.map((header) => {
                    const meta = header.column.columnDef.meta
                    const sortKey = meta?.sortKey
                    const sorted = ariaSort(sortKey)
                    const content = header.isPlaceholder
                      ? null
                      : flexRender(header.column.columnDef.header, header.getContext())
                    return (
                      <TableHead
                        key={header.id}
                        scope="col"
                        aria-sort={sorted}
                        className={cn(meta?.align === 'right' && 'text-right', meta?.className)}
                      >
                        {sortKey !== undefined && onSortChange ? (
                          <button
                            type="button"
                            className="focus-inset -mx-1 inline-flex items-center gap-1 rounded-sm px-1 py-0.5 hover:text-foreground"
                            onClick={() => toggleSort(sortKey)}
                            aria-label={`${t('table.sortBy', { column: meta?.label ?? header.id })}, ${
                              sorted === 'ascending'
                                ? t('table.sortedAscending')
                                : sorted === 'descending'
                                  ? t('table.sortedDescending')
                                  : t('table.notSorted')
                            }`}
                          >
                            {content}
                            {sorted === 'ascending' ? (
                              <ArrowUp className="size-3.5" aria-hidden="true" />
                            ) : sorted === 'descending' ? (
                              <ArrowDown className="size-3.5" aria-hidden="true" />
                            ) : (
                              <ArrowUpDown className="size-3.5 opacity-60" aria-hidden="true" />
                            )}
                          </button>
                        ) : (
                          content
                        )}
                      </TableHead>
                    )
                  })}
                </TableRow>
              ))}
            </TableHeader>
            <TableBody>{body}</TableBody>
          </Table>
        </>
      )}

      {hasRows ? (
        <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
          <p role="status">
            {t('table.rowsLoaded', { count: rows.length })}
            {!hasNextPage ? ` - ${t('table.allLoaded')}` : ''}
          </p>
          <div className="flex items-center gap-3">
            {onPageSizeChange ? (
              <label className="flex items-center gap-2">
                <span className="whitespace-nowrap">{t('table.pageSize')}</span>
                <Select
                  value={String(pageSize ?? prefs.pageSize)}
                  className="w-20"
                  onChange={(e) => onPageSizeChange(Number(e.target.value))}
                >
                  {PAGE_SIZES.map((size) => (
                    <option key={size} value={size}>
                      {size}
                    </option>
                  ))}
                </Select>
              </label>
            ) : null}
            {hasNextPage && onLoadMore ? (
              <Button variant="outline" loading={isLoadingMore} onClick={onLoadMore}>
                {isLoadingMore ? t('table.loadingMore') : t('table.loadMore')}
              </Button>
            ) : null}
          </div>
        </div>
      ) : null}
    </div>
  )
}
