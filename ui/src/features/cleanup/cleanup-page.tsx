import { useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ListChecks, Pencil, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { ConfirmDialog } from '@/components/confirm-dialog'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { RunStatusBadge } from '@/features/system/run-status'
import { useSearchParam, useSearchPatch } from '@/features/system/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import { toast } from '@/lib/toast-store'
import { cleanupKeys, deletePolicy, fetchPolicyPage } from './api'
import { describeCriteria, type CleanupPolicyView } from './criteria'
import { PolicyEditorView } from './policy-editor'

const FILTER_IDS = ['q', 'format'] as const

const FORMAT_OPTIONS = (['maven2', 'npm', 'pypi', 'docker', 'helm', 'raw', 'pub'] as const).map(
  (value) => ({ value, label: t(`cleanup.format.${value}`) }),
)

const FILTERS: readonly FilterDef[] = [
  {
    id: 'q',
    type: 'search',
    label: t('cleanup.filters.name'),
    placeholder: t('cleanup.filters.namePlaceholder'),
  },
  { id: 'format', type: 'select', label: t('cleanup.filters.format'), options: FORMAT_OPTIONS },
]

function LastRun({ policy }: { policy: CleanupPolicyView }) {
  const run = policy.last_run
  if (run === null)
    return <span className="text-muted-foreground">{t('cleanup.lastRun.none')}</span>
  return (
    <div className="flex flex-col items-start gap-1 py-1">
      <span className="flex items-center gap-2">
        <RunStatusBadge status={run.status} />
        <Link
          to="/admin/tasks"
          search={{ view: 'lifecycle', source: 'cleanup', run: run.id }}
          className="text-link underline-offset-2 hover:underline"
        >
          {run.dry_run ? t('cleanup.lastRun.dry') : t('cleanup.lastRun.real')}
        </Link>
      </span>
      <span className="text-xs text-muted-foreground">
        {run.candidates !== null
          ? t('cleanup.lastRun.candidates', {
              count: run.candidates.toLocaleString(),
              size: formatBytes(run.bytes),
            })
          : null}{' '}
        <RelativeTime value={run.finished_at} />
      </span>
    </div>
  )
}

function useColumns(
  onDelete: (policy: CleanupPolicyView) => void,
): AnyColumnDef<CleanupPolicyView>[] {
  return useMemo<AnyColumnDef<CleanupPolicyView>[]>(
    () => [
      {
        accessorKey: 'name',
        header: t('cleanup.columns.name'),
        meta: { label: t('cleanup.columns.name'), hideable: false },
        cell: ({ row }) => (
          <Link
            to="/admin/cleanup-policies"
            search={{ edit: row.original.id }}
            className="font-mono text-link underline-offset-2 hover:underline"
          >
            {row.original.name}
          </Link>
        ),
      },
      {
        accessorKey: 'format',
        header: t('cleanup.columns.format'),
        meta: { label: t('cleanup.columns.format') },
        cell: ({ row }) => <Badge>{row.original.format}</Badge>,
      },
      {
        id: 'criteria',
        header: t('cleanup.columns.criteria'),
        meta: { label: t('cleanup.columns.criteria'), className: 'min-w-64 whitespace-normal' },
        cell: ({ row }) => (
          <ul className="space-y-0.5 py-1">
            {describeCriteria(row.original.criteria).map((part) => (
              <li key={part}>{part}</li>
            ))}
          </ul>
        ),
      },
      {
        id: 'repositories',
        header: t('cleanup.columns.repositories'),
        meta: { label: t('cleanup.columns.repositories'), className: 'min-w-40 whitespace-normal' },
        cell: ({ row }) =>
          row.original.attached_repositories.length === 0 ? (
            <span className="text-muted-foreground">{t('cleanup.noRepositories')}</span>
          ) : (
            <span className="font-mono text-xs">
              {row.original.attached_repositories.map((r) => r.name).join(', ')}
            </span>
          ),
      },
      {
        id: 'lastRun',
        header: t('cleanup.columns.lastRun'),
        meta: { label: t('cleanup.columns.lastRun') },
        cell: ({ row }) => <LastRun policy={row.original} />,
      },
      {
        accessorKey: 'updated_at',
        header: t('cleanup.columns.updated'),
        meta: { label: t('cleanup.columns.updated') },
        cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">{t('cleanup.columns.actions')}</span>,
        enableHiding: false,
        meta: { hideable: false, className: 'w-24' },
        cell: ({ row }) => (
          <div className="flex gap-1">
            <Button asChild variant="ghost" size="icon">
              <Link
                to="/admin/cleanup-policies"
                search={{ edit: row.original.id }}
                aria-label={t('cleanup.actions.edit', { name: row.original.name })}
              >
                <Pencil aria-hidden="true" />
              </Link>
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t('cleanup.actions.delete', { name: row.original.name })}
              onClick={() => onDelete(row.original)}
            >
              <Trash2 aria-hidden="true" />
            </Button>
          </div>
        ),
      },
    ],
    [onDelete],
  )
}

function PolicyList() {
  const url = useUrlTableState(FILTER_IDS)
  const patchSearch = useSearchPatch()
  const queryClient = useQueryClient()
  const { pageSize, setPageSize } = useTablePrefs('cleanup-policies')
  const [toDelete, setToDelete] = useState<CleanupPolicyView | null>(null)
  const { q, format } = url.filters
  const columns = useColumns(setToDelete)

  const list = useInfinitePage<CleanupPolicyView>({
    queryKey: [...cleanupKeys.list({ q, format }), pageSize],
    fetchPage: ({ after, signal }) => fetchPolicyPage({ q, format }, after, pageSize, signal),
  })

  const attached = toDelete?.attached_repositories ?? []
  async function confirmDelete(): Promise<void> {
    if (toDelete === null) return
    await deletePolicy(toDelete.id, { force: attached.length > 0, etag: null })
    await queryClient.invalidateQueries({ queryKey: cleanupKeys.all })
    toast.success(t('cleanup.delete.done', { name: toDelete.name }))
  }

  const openCreate = () => patchSearch({ edit: 'new' })
  return (
    <>
      <PageHeader
        title={t('cleanup.title')}
        description={t('cleanup.description')}
        actions={
          <>
            <Button asChild variant="outline">
              <Link to="/admin/tasks" search={{ view: 'lifecycle', source: 'cleanup' }}>
                {t('cleanup.actions.openRuns')}
              </Link>
            </Button>
            <PrivilegeGate need="admin">
              <Button onClick={openCreate}>
                <Plus aria-hidden="true" />
                {t('cleanup.create')}
              </Button>
            </PrivilegeGate>
          </>
        }
      />
      <DataTable<CleanupPolicyView>
        tableId="cleanup-policies"
        label={t('cleanup.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(row) => row.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        hasActiveFilters={url.hasActiveFilters}
        onClearFilters={url.clearFilters}
        pageSize={pageSize}
        onPageSizeChange={setPageSize}
        toolbar={
          <FilterBar
            filters={FILTERS}
            values={url.filters}
            onChange={url.setFilter}
            onClear={url.clearFilters}
          />
        }
        emptyState={
          <EmptyState
            icon={ListChecks}
            title={t('cleanup.empty.title')}
            description={t('cleanup.empty.description')}
            action={
              <PrivilegeGate need="admin">
                <Button onClick={openCreate}>
                  <Plus aria-hidden="true" />
                  {t('cleanup.create')}
                </Button>
              </PrivilegeGate>
            }
          />
        }
      />
      {toDelete ? (
        <ConfirmDialog
          open
          onOpenChange={(open) => !open && setToDelete(null)}
          kind="cleanup policy"
          name={toDelete.name}
          description={
            attached.length > 0
              ? t('cleanup.delete.inUse', {
                  count: attached.length,
                  names: attached.map((r) => r.name).join(', '),
                })
              : t('cleanup.delete.description')
          }
          dependents={attached.map((r) => ({ kind: 'repository', id: r.id, name: r.name }))}
          confirmLabel={attached.length > 0 ? t('cleanup.delete.confirmDetach') : undefined}
          requireTyping
          onConfirm={confirmDelete}
        />
      ) : null}
    </>
  )
}

/** `/admin/cleanup-policies`: the list, or the editor of one policy while `?edit=` is set. */
export function CleanupPage() {
  const edit = useSearchParam('edit')
  return edit ? <PolicyEditorView policyId={edit} /> : <PolicyList />
}
