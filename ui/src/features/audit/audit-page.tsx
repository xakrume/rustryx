import { Download, RefreshCw } from 'lucide-react'
import { useCallback, useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { CopyButton } from '@/components/copy-button'
import { DataTable, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { formatDateTime, parseDate } from '@/lib/format'
import { auditPage, resolveBounds, type AuditEvent, type AuditFilters } from './api'
import { AuditToolbar } from './audit-toolbar'
import { EventDrawer } from './event-drawer'
import { ExportDialog } from './export-dialog'
import { KindBadge } from './kind-badge'

const FILTER_IDS = [
  'kind',
  'actor',
  'action',
  'target',
  'repository',
  'project',
  'request_id',
  'outcome',
  'range',
  'from',
  'to',
] as const
const DIALOG_PARAMS = ['event', 'export'] as const

/**
 * The audit log: filters by kind, actor, action, target, outcome, repository, project and time,
 * keyset pagination, a drawer with the full event and its JSON detail, and an NDJSON export that
 * streams to a file. Times are absolute and local (an audit trail needs the exact moment), with
 * the UTC instant in the drawer.
 */
export function AuditPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('audit')
  const [eventId, setEventId] = useSearchParam('event')
  const [exporting, setExporting] = useSearchParam('export')
  const closeDialogs = useClearSearchParams(DIALOG_PARAMS)
  // A relative range is anchored when it is chosen or refreshed, see `resolveBounds`.
  const [anchor, setAnchor] = useState(() => Date.now())
  const filters = useMemo<AuditFilters>(
    () => ({
      kind: state.filters.kind,
      actor: state.filters.actor,
      action: state.filters.action,
      target: state.filters.target,
      repository: state.filters.repository,
      project: state.filters.project,
      request_id: state.filters.request_id,
      outcome: state.filters.outcome,
      range: state.filters.range,
      from: state.filters.from,
      to: state.filters.to,
    }),
    [state.filters],
  )
  const bounds = useMemo(() => resolveBounds(filters, anchor), [filters, anchor])
  const sort = state.sort === 'at' ? 'at' : '-at'
  const list = useInfinitePage(auditPage(filters, bounds, sort, prefs.pageSize))

  const setFilter = useCallback(
    (id: string, value: string | null) => {
      // Choosing a relative range starts its window now.
      if (id === 'range') setAnchor(Date.now())
      state.setFilter(id, value)
    },
    [state],
  )

  const columns = useMemo<AnyColumnDef<AuditEvent>[]>(
    () => [
      {
        id: 'time',
        header: t('auditLog.columns.time'),
        meta: { label: t('auditLog.columns.time'), sortKey: 'at', hideable: false },
        cell: ({ row }) => {
          const date = parseDate(row.original.at)
          const text = date ? formatDateTime(date) : row.original.at
          return (
            <button
              type="button"
              onClick={() => setEventId(row.original.id)}
              aria-label={t('auditLog.openEvent', { action: row.original.action, time: text })}
              className="focus-inset rounded-sm font-mono text-xs text-link underline-offset-2 hover:underline"
            >
              {date ? (
                <time dateTime={date.toISOString()} title={date.toISOString()}>
                  {text}
                </time>
              ) : (
                text
              )}
            </button>
          )
        },
      },
      {
        id: 'kind',
        header: t('auditLog.columns.kind'),
        meta: { label: t('auditLog.columns.kind') },
        cell: ({ row }) => <KindBadge kind={row.original.kind} />,
      },
      {
        id: 'actor',
        header: t('auditLog.columns.actor'),
        meta: { label: t('auditLog.columns.actor') },
        cell: ({ row }) => <span className="font-mono">{row.original.actor_name}</span>,
      },
      {
        id: 'action',
        header: t('auditLog.columns.action'),
        meta: { label: t('auditLog.columns.action') },
        cell: ({ row }) => <span className="font-mono">{row.original.action}</span>,
      },
      {
        id: 'target',
        header: t('auditLog.columns.target'),
        meta: { label: t('auditLog.columns.target') },
        cell: ({ row }) =>
          row.original.target === null ? (
            <span className="text-muted-foreground">{t('auditLog.noTarget')}</span>
          ) : (
            <span
              className="inline-block max-w-56 truncate align-bottom font-mono"
              title={row.original.target}
            >
              {row.original.target}
            </span>
          ),
      },
      {
        id: 'outcome',
        header: t('auditLog.columns.outcome'),
        meta: { label: t('auditLog.columns.outcome') },
        cell: ({ row }) => (
          <StatusBadge
            status={row.original.outcome === 'success' ? 'ok' : 'error'}
            label={t(`auditLog.outcome.${row.original.outcome}`)}
          />
        ),
      },
      {
        id: 'requestId',
        header: t('auditLog.columns.requestId'),
        meta: { label: t('auditLog.columns.requestId') },
        cell: ({ row }) =>
          row.original.request_id === null ? (
            <span className="text-muted-foreground">{t('auditLog.noTarget')}</span>
          ) : (
            <span className="inline-flex items-center gap-1">
              <span className="font-mono text-xs">{row.original.request_id.slice(-12)}</span>
              <CopyButton
                value={row.original.request_id}
                what={t('auditLog.columns.requestId')}
                size="icon"
                className="size-6"
              />
            </span>
          ),
      },
    ],
    [setEventId],
  )

  return (
    <>
      <PageHeader
        title={t('nav.items.auditLog')}
        description={t('auditLog.description')}
        actions={
          <>
            <Button
              variant="outline"
              onClick={() => {
                setAnchor(Date.now())
                list.refetch()
              }}
            >
              <RefreshCw aria-hidden="true" />
              {t('auditLog.refresh')}
            </Button>
            <Button variant="outline" onClick={() => setExporting('1')}>
              <Download aria-hidden="true" />
              {t('auditLog.exportButton')}
            </Button>
          </>
        }
      />
      <DataTable
        tableId="audit"
        label={t('auditLog.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(e) => e.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        sort={sort}
        onSortChange={state.setSort}
        pageSize={prefs.pageSize}
        onPageSizeChange={prefs.setPageSize}
        hasActiveFilters={state.hasActiveFilters}
        onClearFilters={state.clearFilters}
        emptyState={
          <EmptyState title={t('auditLog.empty.title')} description={t('auditLog.empty.body')} />
        }
        toolbar={
          <div className="space-y-2">
            <AuditToolbar
              values={state.filters}
              onChange={setFilter}
              onClear={state.clearFilters}
            />
            <p className="text-xs text-muted-foreground">{t('auditLog.retention')}</p>
          </div>
        }
      />
      {eventId !== undefined ? (
        <EventDrawer
          eventId={eventId}
          known={list.items.find((e) => e.id === eventId)}
          onClose={closeDialogs}
        />
      ) : null}
      {exporting !== undefined ? (
        <ExportDialog filters={filters} bounds={bounds} onClose={closeDialogs} />
      ) : null}
    </>
  )
}
