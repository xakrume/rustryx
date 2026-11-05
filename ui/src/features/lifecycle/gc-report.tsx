import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { BytesText } from '@/components/bytes-text'
import { DataTable, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { RunStatusBadge } from '@/features/system/run-status'
import { t } from '@/i18n'
import { fetchGcCandidatePage, gcRunQuery, lifecycleKeys, type GcCandidate } from './api'

function columns(): AnyColumnDef<GcCandidate>[] {
  return [
    {
      accessorKey: 'blob_store',
      header: t('lifecycle.report.gc.store'),
      meta: { label: t('lifecycle.report.gc.store') },
      cell: ({ row }) => <span className="font-mono">{row.original.blob_store}</span>,
    },
    {
      accessorKey: 'key',
      header: t('lifecycle.report.gc.key'),
      meta: { label: t('lifecycle.report.gc.key'), hideable: false },
      cell: ({ row }) => <span className="font-mono">{row.original.key}</span>,
    },
    {
      id: 'bytes',
      header: t('lifecycle.report.gc.bytes'),
      meta: { label: t('lifecycle.report.gc.bytes'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.size_bytes} />,
    },
    {
      id: 'orphaned',
      header: t('lifecycle.report.gc.orphanedAt'),
      meta: { label: t('lifecycle.report.gc.orphanedAt') },
      cell: ({ row }) => <RelativeTime value={row.original.orphaned_at} />,
    },
  ]
}

function GcSample({ runId }: { runId: string }) {
  const { pageSize, setPageSize } = useTablePrefs('gc-candidates')
  const cols = useMemo(columns, [])
  const list = useInfinitePage<GcCandidate>({
    queryKey: [
      ...lifecycleKeys.candidates('gc', runId, { repository: undefined, protected: undefined }),
      pageSize,
    ],
    fetchPage: ({ after, signal }) => fetchGcCandidatePage(runId, after, pageSize, signal),
  })
  return (
    <DataTable<GcCandidate>
      tableId="gc-candidates"
      label={t('lifecycle.report.gc.sample')}
      columns={cols}
      rows={list.items}
      getRowId={(row) => `${row.blob_store}:${row.key}`}
      isLoading={list.isLoading}
      error={list.isError ? list.error : undefined}
      onRetry={list.refetch}
      hasNextPage={list.hasNextPage}
      isLoadingMore={list.isLoadingMore}
      onLoadMore={list.loadMore}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      emptyState={<EmptyState title={t('lifecycle.report.emptyPlan')} />}
    />
  )
}

/** Report of one garbage collection run: totals, the split by blob store, a sample of blobs. */
export function GcReport({ runId }: { runId: string }) {
  const query = useQuery(gcRunQuery(runId))
  const run = query.data
  if (query.isError && !run) {
    return <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
  }
  if (!run) return <Skeleton className="h-40" aria-busy="true" />
  const active = run.status === 'queued' || run.status === 'running'
  return (
    <section aria-labelledby={`gc-${run.id}`} className="space-y-3">
      <h2 id={`gc-${run.id}`} className="flex flex-wrap items-center gap-2 text-base font-semibold">
        {t('lifecycle.report.titleGc')}
        <RunStatusBadge status={run.status} />
        <Badge>{run.dry_run ? t('lifecycle.mode.dry') : t('lifecycle.mode.real')}</Badge>
      </h2>
      <p className="text-sm text-muted-foreground">
        {t('lifecycle.report.gc.options', {
          untagged: run.delete_untagged ? t('common.yes') : t('common.no'),
          workers: run.workers,
        })}
      </p>
      {active ? (
        <Alert variant="info" title={t('lifecycle.report.planning')}>
          {t('lifecycle.report.planningHint')}
        </Alert>
      ) : null}
      <dl className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <div className="rounded-xl border border-transparent bg-card dark:border-border/60 px-4 py-3">
          <dt className="text-xs font-medium text-muted-foreground">
            {t('lifecycle.report.gc.stats.blobs')}
          </dt>
          <dd className="mt-1 text-2xl font-semibold tabular-nums">
            {run.candidate_blobs?.toLocaleString() ?? '-'}
          </dd>
        </div>
        <div className="rounded-xl border border-transparent bg-card dark:border-border/60 px-4 py-3">
          <dt className="text-xs font-medium text-muted-foreground">
            {t('lifecycle.report.gc.stats.bytes')}
          </dt>
          <dd className="mt-1 text-2xl font-semibold tabular-nums">
            <BytesText bytes={run.reclaimed_bytes} />
          </dd>
        </div>
      </dl>
      <p className="text-xs text-muted-foreground">{t('lifecycle.report.gc.estimate')}</p>
      {run.per_blob_store.length > 0 ? (
        <Card>
          <CardContent className="pt-4">
            <h3 className="mb-2 text-sm font-semibold">{t('lifecycle.report.gc.perStore')}</h3>
            <table className="w-full text-left text-(length:--text-table)">
              <thead>
                <tr className="border-b border-border text-muted-foreground">
                  <th scope="col" className="py-1 pr-2 font-medium">
                    {t('lifecycle.report.gc.store')}
                  </th>
                  <th scope="col" className="py-1 pr-2 text-right font-medium">
                    {t('lifecycle.report.gc.blobs')}
                  </th>
                  <th scope="col" className="py-1 text-right font-medium">
                    {t('lifecycle.report.gc.bytes')}
                  </th>
                </tr>
              </thead>
              <tbody>
                {run.per_blob_store.map((store) => (
                  <tr key={store.blob_store} className="border-b border-border last:border-0">
                    <th scope="row" className="py-1 pr-2 font-mono font-normal">
                      {store.blob_store}
                    </th>
                    <td className="py-1 pr-2 text-right tabular-nums">
                      {store.candidates.toLocaleString()}
                    </td>
                    <td className="py-1 text-right tabular-nums">
                      <BytesText bytes={store.bytes} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </CardContent>
        </Card>
      ) : null}
      {run.status === 'succeeded' && (run.candidate_blobs ?? 0) > 0 ? (
        <GcSample runId={run.id} />
      ) : null}
    </section>
  )
}
