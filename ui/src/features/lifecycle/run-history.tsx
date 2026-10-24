import { useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { BytesText } from '@/components/bytes-text'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Select } from '@/components/ui/select'
import { RunStatusBadge } from '@/features/system/run-status'
import { t } from '@/i18n'
import { formatDuration, parseDate } from '@/lib/format'
import {
  fetchGcRunPage,
  fetchLifecycleRunPage,
  lifecycleKeys,
  type GcRun,
  type LifecycleRun,
  type RunFilters,
  type RunSource,
} from './api'

const seconds = (from: string | null, to: string | null): string => {
  const a = parseDate(from)
  const b = parseDate(to)
  return a && b ? formatDuration((b.getTime() - a.getTime()) / 1000) : '-'
}

function filtersFor(source: RunSource): readonly FilterDef[] {
  // Cleanup runs can be cancelled, garbage collection runs are stopped (contract, section 14).
  const statuses =
    source === 'gc'
      ? ['queued', 'running', 'succeeded', 'failed', 'stopped']
      : ['queued', 'running', 'succeeded', 'failed', 'cancelled']
  return [
    {
      id: 'mode',
      type: 'select',
      label: t('lifecycle.filters.mode'),
      options: [
        { value: 'dry', label: t('lifecycle.mode.dry') },
        { value: 'real', label: t('lifecycle.mode.real') },
      ],
    },
    {
      id: 'status',
      type: 'select',
      label: t('lifecycle.filters.status'),
      options: statuses.map((value) => ({ value, label: t(`runStatus.${value as 'queued'}`) })),
    },
  ]
}

function modeBadge(dryRun: boolean) {
  return <Badge>{dryRun ? t('lifecycle.mode.dry') : t('lifecycle.mode.real')}</Badge>
}

function cleanupColumns(onOpen: (id: string) => void): AnyColumnDef<LifecycleRun>[] {
  return [
    {
      id: 'policy',
      header: t('lifecycle.columns.policy'),
      meta: { label: t('lifecycle.columns.policy'), hideable: false },
      cell: ({ row }) => (
        <Button
          variant="link"
          className="h-auto p-0 font-mono"
          aria-label={t('lifecycle.openReport', {
            name: row.original.policy_name ?? row.original.id,
          })}
          onClick={() => onOpen(row.original.id)}
        >
          {row.original.policy_name ?? row.original.id}
        </Button>
      ),
    },
    {
      id: 'mode',
      header: t('lifecycle.columns.mode'),
      meta: { label: t('lifecycle.columns.mode') },
      cell: ({ row }) => modeBadge(row.original.dry_run),
    },
    {
      id: 'status',
      header: t('lifecycle.columns.status'),
      meta: { label: t('lifecycle.columns.status') },
      cell: ({ row }) => <RunStatusBadge status={row.original.status} />,
    },
    {
      id: 'trigger',
      header: t('lifecycle.columns.trigger'),
      meta: { label: t('lifecycle.columns.trigger') },
      cell: ({ row }) => t(`lifecycle.trigger.${row.original.trigger}`),
    },
    {
      id: 'queued',
      header: t('lifecycle.columns.queued'),
      meta: { label: t('lifecycle.columns.queued') },
      cell: ({ row }) => <RelativeTime value={row.original.queued_at} />,
    },
    {
      id: 'duration',
      header: t('lifecycle.columns.duration'),
      meta: { label: t('lifecycle.columns.duration'), align: 'right' },
      cell: ({ row }) => seconds(row.original.started_at, row.original.finished_at),
    },
    {
      id: 'candidates',
      header: t('lifecycle.columns.candidates'),
      meta: { label: t('lifecycle.columns.candidates'), align: 'right' },
      cell: ({ row }) => row.original.summary?.candidates.toLocaleString() ?? '-',
    },
    {
      id: 'size',
      header: t('lifecycle.columns.size'),
      meta: { label: t('lifecycle.columns.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.summary?.bytes} />,
    },
    {
      id: 'expires',
      header: t('lifecycle.columns.expires'),
      meta: { label: t('lifecycle.columns.expires') },
      cell: ({ row }) => <RelativeTime value={row.original.report_expires_at} />,
    },
  ]
}

function gcColumns(onOpen: (id: string) => void): AnyColumnDef<GcRun>[] {
  return [
    {
      id: 'run',
      header: t('lifecycle.columns.open'),
      meta: { label: t('lifecycle.columns.open'), hideable: false },
      cell: ({ row }) => (
        <Button
          variant="link"
          className="h-auto p-0"
          aria-label={t('lifecycle.openReport', { name: row.original.id })}
          onClick={() => onOpen(row.original.id)}
        >
          <RelativeTime value={row.original.queued_at} />
        </Button>
      ),
    },
    {
      id: 'mode',
      header: t('lifecycle.columns.mode'),
      meta: { label: t('lifecycle.columns.mode') },
      cell: ({ row }) => modeBadge(row.original.dry_run),
    },
    {
      id: 'status',
      header: t('lifecycle.columns.status'),
      meta: { label: t('lifecycle.columns.status') },
      cell: ({ row }) => <RunStatusBadge status={row.original.status} />,
    },
    {
      id: 'trigger',
      header: t('lifecycle.columns.trigger'),
      meta: { label: t('lifecycle.columns.trigger') },
      cell: ({ row }) => t(`lifecycle.trigger.${row.original.trigger}`),
    },
    {
      id: 'duration',
      header: t('lifecycle.columns.duration'),
      meta: { label: t('lifecycle.columns.duration'), align: 'right' },
      cell: ({ row }) => seconds(row.original.started_at, row.original.finished_at),
    },
    {
      id: 'blobs',
      header: t('lifecycle.columns.blobs'),
      meta: { label: t('lifecycle.columns.blobs'), align: 'right' },
      cell: ({ row }) => row.original.candidate_blobs?.toLocaleString() ?? '-',
    },
    {
      id: 'reclaimed',
      header: t('lifecycle.columns.reclaimed'),
      meta: { label: t('lifecycle.columns.reclaimed'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.reclaimed_bytes} />,
    },
  ]
}

export interface RunHistoryProps {
  source: RunSource
  onSourceChange: (source: RunSource) => void
  onOpen: (id: string) => void
  filters: Pick<RunFilters, 'mode' | 'status'>
  onFilterChange: (id: string, value: string | null) => void
  onClearFilters: () => void
}

/**
 * History of cleanup runs and of garbage collection runs, newest first. Filters and the run
 * kind are owned by the page (address bar); opening a run shows its report.
 */
export function RunHistory({
  source,
  onSourceChange,
  onOpen,
  filters,
  onFilterChange,
  onClearFilters,
}: RunHistoryProps) {
  const { pageSize, setPageSize } = useTablePrefs(`lifecycle-runs-${source}`)
  const cleanupCols = useMemo(() => cleanupColumns(onOpen), [onOpen])
  const gcCols = useMemo(() => gcColumns(onOpen), [onOpen])
  const runFilters: RunFilters = { mode: filters.mode, status: filters.status }
  const hasFilters = filters.mode !== undefined || filters.status !== undefined
  const statusFilters = useMemo(() => filtersFor(source), [source])

  const cleanup = useInfinitePage<LifecycleRun>({
    queryKey: [...lifecycleKeys.runs('cleanup', runFilters), pageSize],
    fetchPage: ({ after, signal }) => fetchLifecycleRunPage(runFilters, after, pageSize, signal),
    enabled: source === 'cleanup',
  })
  const gc = useInfinitePage<GcRun>({
    queryKey: [...lifecycleKeys.runs('gc', runFilters), pageSize],
    fetchPage: ({ after, signal }) => fetchGcRunPage(runFilters, after, pageSize, signal),
    enabled: source === 'gc',
  })

  const sourceSelect = (
    <label className="flex flex-col gap-1 text-xs font-medium">
      <span>{t('lifecycle.source.label')}</span>
      <Select
        value={source}
        className="min-w-48"
        onChange={(event) => onSourceChange(event.target.value === 'gc' ? 'gc' : 'cleanup')}
      >
        <option value="cleanup">{t('lifecycle.source.cleanup')}</option>
        <option value="gc">{t('lifecycle.source.gc')}</option>
      </Select>
    </label>
  )

  const toolbar = (
    <div className="flex flex-wrap items-end gap-3">
      {sourceSelect}
      <div className="min-w-0 flex-1">
        <FilterBar
          filters={statusFilters}
          values={{ mode: filters.mode, status: filters.status }}
          onChange={onFilterChange}
          onClear={onClearFilters}
        />
      </div>
    </div>
  )

  return source === 'cleanup' ? (
    <DataTable<LifecycleRun>
      key="cleanup"
      tableId="lifecycle-runs-cleanup"
      label={t('lifecycle.tableLabel')}
      columns={cleanupCols}
      rows={cleanup.items}
      getRowId={(row) => row.id}
      isLoading={cleanup.isLoading}
      error={cleanup.isError ? cleanup.error : undefined}
      onRetry={cleanup.refetch}
      hasNextPage={cleanup.hasNextPage}
      isLoadingMore={cleanup.isLoadingMore}
      onLoadMore={cleanup.loadMore}
      hasActiveFilters={hasFilters}
      onClearFilters={onClearFilters}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      toolbar={toolbar}
      emptyState={
        <p className="rounded-lg border border-dashed border-input p-8 text-center text-sm text-muted-foreground">
          <span className="block text-base font-semibold text-foreground">
            {t('lifecycle.empty.title')}
          </span>
          {t('lifecycle.empty.description')}
        </p>
      }
    />
  ) : (
    <DataTable<GcRun>
      key="gc"
      tableId="lifecycle-runs-gc"
      label={t('lifecycle.tableLabel')}
      columns={gcCols}
      rows={gc.items}
      getRowId={(row) => row.id}
      isLoading={gc.isLoading}
      error={gc.isError ? gc.error : undefined}
      onRetry={gc.refetch}
      hasNextPage={gc.hasNextPage}
      isLoadingMore={gc.isLoadingMore}
      onLoadMore={gc.loadMore}
      hasActiveFilters={hasFilters}
      onClearFilters={onClearFilters}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      toolbar={toolbar}
      emptyState={
        <p className="rounded-lg border border-dashed border-input p-8 text-center text-sm text-muted-foreground">
          {t('lifecycle.empty.gc')}
        </p>
      }
    />
  )
}
