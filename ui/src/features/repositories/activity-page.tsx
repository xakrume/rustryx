import { useParams } from '@tanstack/react-router'
import { History } from 'lucide-react'
import { useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { CopyButton } from '@/components/copy-button'
import { DataTable, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { t } from '@/i18n'
import { activityConfig, type AuditEvent } from './api'

function columns(): AnyColumnDef<AuditEvent>[] {
  return [
    {
      id: 'at',
      header: t('repos.activity.when'),
      meta: { label: t('repos.activity.when'), hideable: false },
      cell: ({ row }) => <RelativeTime value={row.original.at} />,
    },
    {
      id: 'actor',
      header: t('repos.activity.who'),
      meta: { label: t('repos.activity.who') },
      cell: ({ row }) => row.original.actor_name,
    },
    {
      id: 'action',
      header: t('repos.activity.action'),
      meta: { label: t('repos.activity.action') },
      cell: ({ row }) => <span className="font-mono">{row.original.action}</span>,
    },
    {
      id: 'target',
      header: t('repos.activity.target'),
      meta: { label: t('repos.activity.target') },
      cell: ({ row }) => (
        <span
          className="block max-w-80 truncate font-mono"
          title={row.original.target ?? undefined}
        >
          {row.original.target ?? '-'}
        </span>
      ),
    },
    {
      id: 'outcome',
      header: t('repos.activity.outcome'),
      meta: { label: t('repos.activity.outcome') },
      cell: ({ row }) => (
        <StatusBadge
          status={row.original.outcome === 'success' ? 'ok' : 'error'}
          label={
            row.original.outcome === 'success'
              ? t('repos.activity.success')
              : t('repos.activity.failure')
          }
        />
      ),
    },
    {
      id: 'request',
      header: t('common.requestId'),
      meta: { label: t('common.requestId') },
      cell: ({ row }) =>
        row.original.request_id ? (
          <span className="flex items-center gap-1">
            <span className="font-mono text-muted-foreground">
              {row.original.request_id.slice(-8)}
            </span>
            <CopyButton value={row.original.request_id} what={t('common.requestId')} />
          </span>
        ) : (
          '-'
        ),
    },
  ]
}

/** Activity tab: audit events of this repository, newest first (who published, deleted, changed settings). */
export function RepositoryActivityPage() {
  const { name } = useParams({ from: '/_app/repositories/$name/activity' })
  const list = useInfinitePage<AuditEvent>(activityConfig(name))
  const cols = useMemo(columns, [])
  return (
    <div className="space-y-3">
      <h2 className="sr-only">{t('pages.activity')}</h2>
      <p className="text-sm text-muted-foreground">{t('repos.activity.description')}</p>
      <DataTable<AuditEvent>
        tableId="repository-activity"
        label={t('pages.activity')}
        columns={cols}
        rows={list.items}
        getRowId={(e) => e.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        emptyState={
          <EmptyState
            icon={History}
            title={t('repos.activity.emptyTitle')}
            description={t('repos.activity.emptyBody')}
          />
        }
      />
    </div>
  )
}
