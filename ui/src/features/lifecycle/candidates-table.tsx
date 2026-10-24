import { useEffect, useMemo, useState } from 'react'
import { isApiError } from '@/api/errors'
import { useInfinitePage } from '@/api/pagination'
import { BytesText } from '@/components/bytes-text'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { RelativeTime } from '@/components/relative-time'
import { t } from '@/i18n'
import {
  fetchCandidatePage,
  lifecycleKeys,
  type CandidateFilters,
  type LifecycleCandidate,
} from './api'

const NO_FILTERS: CandidateFilters = { repository: undefined, protected: undefined }

function columns(): AnyColumnDef<LifecycleCandidate>[] {
  return [
    {
      accessorKey: 'repository',
      header: t('lifecycle.report.columns.repository'),
      meta: { label: t('lifecycle.report.columns.repository') },
      cell: ({ row }) => <span className="font-mono">{row.original.repository}</span>,
    },
    {
      accessorKey: 'coordinate',
      header: t('lifecycle.report.columns.coordinate'),
      meta: { label: t('lifecycle.report.columns.coordinate'), hideable: false },
      cell: ({ row }) => <span className="font-mono">{row.original.coordinate}</span>,
    },
    {
      id: 'size',
      header: t('lifecycle.report.columns.size'),
      meta: { label: t('lifecycle.report.columns.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.size_bytes} />,
    },
    {
      id: 'age',
      header: t('lifecycle.report.columns.age'),
      meta: { label: t('lifecycle.report.columns.age'), align: 'right' },
      cell: ({ row }) => t('lifecycle.report.ageDays', { count: row.original.age_days }),
    },
    {
      id: 'lastDownloaded',
      header: t('lifecycle.report.columns.lastDownloaded'),
      meta: { label: t('lifecycle.report.columns.lastDownloaded') },
      cell: ({ row }) =>
        row.original.last_downloaded_at ? (
          <RelativeTime value={row.original.last_downloaded_at} />
        ) : (
          <span className="text-muted-foreground">{t('lifecycle.report.neverDownloaded')}</span>
        ),
    },
    {
      id: 'reason',
      header: t('lifecycle.report.columns.reason'),
      meta: { label: t('lifecycle.report.columns.reason') },
      cell: ({ row }) => {
        const reason = row.original.reason
        const known = reason in REASON_KEYS
        return known ? t(`lifecycle.report.reason.${reason}`) : reason
      },
    },
    {
      id: 'protectedBy',
      header: t('lifecycle.report.columns.protectedBy'),
      meta: { label: t('lifecycle.report.columns.protectedBy') },
      cell: ({ row }) =>
        row.original.protected_by ? (
          <span className="font-medium">{row.original.protected_by}</span>
        ) : (
          <span className="text-muted-foreground">{t('lifecycle.report.notProtected')}</span>
        ),
    },
  ]
}

const REASON_KEYS = {
  older_than_days: true,
  last_downloaded_days: true,
  keep_last_versions: true,
  name_pattern: true,
  release_type: true,
} as const

export interface CandidatesTableProps {
  runId: string
  /** Offered as the repository filter (the API filters by exact name). */
  repositories?: readonly string[]
  /** The report expired: the server answers 404 for the list. */
  onExpired?: () => void
}

/**
 * The paged plan of a run. The API pages through the stored report (keyset, "load more"); the
 * whole plan is never rendered at once, because a large plan would freeze the page: the CSV
 * export is the way to get the complete list (docs/ui-page-inventory.md, cleanup policies).
 */
export function CandidatesTable({ runId, repositories = [], onExpired }: CandidatesTableProps) {
  const [filters, setFilters] = useState<CandidateFilters>(NO_FILTERS)
  const { pageSize, setPageSize } = useTablePrefs('lifecycle-candidates')
  const cols = useMemo(columns, [])

  const filterDefs = useMemo<readonly FilterDef[]>(
    () => [
      repositories.length > 0
        ? {
            id: 'repository',
            type: 'select',
            label: t('lifecycle.report.filters.repository'),
            options: repositories.map((name) => ({ value: name, label: name })),
          }
        : {
            id: 'repository',
            type: 'search',
            label: t('lifecycle.report.filters.repository'),
          },
      {
        id: 'protected',
        type: 'select',
        label: t('lifecycle.report.filters.protected'),
        options: [
          { value: 'yes', label: t('lifecycle.report.filters.protectedOnly') },
          { value: 'no', label: t('lifecycle.report.filters.unprotectedOnly') },
        ],
      },
    ],
    [repositories],
  )

  const list = useInfinitePage<LifecycleCandidate>({
    queryKey: [...lifecycleKeys.candidates('cleanup', runId, filters), pageSize],
    fetchPage: ({ after, signal }) => fetchCandidatePage(runId, filters, after, pageSize, signal),
  })

  const expired =
    list.isError && isApiError(list.error) && list.error.code === 'lifecycle_run_not_found'
  useEffect(() => {
    if (expired) onExpired?.()
  }, [expired, onExpired])
  if (expired) return null

  const setFilter = (id: string, value: string | null) =>
    setFilters((previous) => ({ ...previous, [id]: value ?? undefined }))
  const hasActive = filters.repository !== undefined || filters.protected !== undefined

  return (
    <DataTable<LifecycleCandidate>
      tableId="lifecycle-candidates"
      label={t('lifecycle.report.candidatesLabel')}
      columns={cols}
      rows={list.items}
      getRowId={(row) =>
        `${row.repository}:${row.coordinate}:${row.asset_id ?? row.component_id ?? ''}`
      }
      isLoading={list.isLoading}
      error={list.isError ? list.error : undefined}
      onRetry={list.refetch}
      hasNextPage={list.hasNextPage}
      isLoadingMore={list.isLoadingMore}
      onLoadMore={list.loadMore}
      hasActiveFilters={hasActive}
      onClearFilters={() => setFilters(NO_FILTERS)}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      toolbar={
        <FilterBar
          filters={filterDefs}
          values={{ repository: filters.repository, protected: filters.protected }}
          onChange={setFilter}
          onClear={() => setFilters(NO_FILTERS)}
        />
      }
      emptyState={<EmptyState title={t('lifecycle.report.emptyPlan')} />}
    />
  )
}
