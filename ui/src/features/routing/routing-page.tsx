import { useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Pencil, Plus, Route as RouteIcon, Trash2 } from 'lucide-react'
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
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { useSearchParam, useSearchPatch } from '@/features/system/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { deleteRule, fetchRulePage, routingKeys, type RoutingRuleView } from './api'
import { RuleEditorView } from './rule-editor'

const FILTER_IDS = ['q'] as const
const FILTERS: readonly FilterDef[] = [
  {
    id: 'q',
    type: 'search',
    label: t('routing.filters.name'),
    placeholder: t('routing.filters.namePlaceholder'),
  },
]

function useColumns(onDelete: (rule: RoutingRuleView) => void): AnyColumnDef<RoutingRuleView>[] {
  return useMemo<AnyColumnDef<RoutingRuleView>[]>(
    () => [
      {
        accessorKey: 'name',
        header: t('routing.columns.name'),
        meta: { label: t('routing.columns.name'), hideable: false },
        cell: ({ row }) => (
          <Link
            to="/admin/routing-rules"
            search={{ edit: row.original.id }}
            className="font-mono text-link underline-offset-2 hover:underline"
          >
            {row.original.name}
          </Link>
        ),
      },
      {
        accessorKey: 'mode',
        header: t('routing.columns.mode'),
        meta: { label: t('routing.columns.mode') },
        cell: ({ row }) =>
          row.original.mode === 'block' ? (
            <StatusBadge status="error" label={t('routing.mode.block')} />
          ) : (
            <StatusBadge status="ok" label={t('routing.mode.allow')} />
          ),
      },
      {
        id: 'patterns',
        header: t('routing.columns.patterns'),
        meta: { label: t('routing.columns.patterns'), className: 'min-w-56 whitespace-normal' },
        cell: ({ row }) => {
          const [first, ...rest] = row.original.patterns
          return (
            <div className="space-y-0.5 py-1">
              <code className="block font-mono text-xs break-all">{first ?? '-'}</code>
              {rest.length > 0 ? (
                <span className="text-xs text-muted-foreground">
                  {t('routing.morePatterns', { count: rest.length })}
                </span>
              ) : null}
            </div>
          )
        },
      },
      {
        id: 'repositories',
        header: t('routing.columns.repositories'),
        meta: { label: t('routing.columns.repositories'), className: 'min-w-40 whitespace-normal' },
        cell: ({ row }) =>
          row.original.assigned_repositories.length === 0 ? (
            <span className="text-muted-foreground">{t('routing.noRepositories')}</span>
          ) : (
            <span className="font-mono text-xs">
              {row.original.assigned_repositories.map((r) => r.name).join(', ')}
            </span>
          ),
      },
      {
        accessorKey: 'updated_at',
        header: t('routing.columns.updated'),
        meta: { label: t('routing.columns.updated') },
        cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">{t('routing.columns.actions')}</span>,
        enableHiding: false,
        meta: { hideable: false, className: 'w-24' },
        cell: ({ row }) => {
          const used = row.original.assigned_repositories.length
          return (
            <div className="flex gap-1">
              <Button asChild variant="ghost" size="icon">
                <Link
                  to="/admin/routing-rules"
                  search={{ edit: row.original.id }}
                  aria-label={t('routing.actions.edit', { name: row.original.name })}
                >
                  <Pencil aria-hidden="true" />
                </Link>
              </Button>
              <Button
                variant="ghost"
                size="icon"
                disabled={used > 0}
                aria-label={
                  used > 0
                    ? t('routing.actions.deleteBlocked', { name: row.original.name, count: used })
                    : t('routing.actions.delete', { name: row.original.name })
                }
                onClick={() => onDelete(row.original)}
              >
                <Trash2 aria-hidden="true" />
              </Button>
            </div>
          )
        },
      },
    ],
    [onDelete],
  )
}

function RuleList() {
  const url = useUrlTableState(FILTER_IDS)
  const patchSearch = useSearchPatch()
  const queryClient = useQueryClient()
  const { pageSize, setPageSize } = useTablePrefs('routing-rules')
  const [toDelete, setToDelete] = useState<RoutingRuleView | null>(null)
  const { q } = url.filters
  const columns = useColumns(setToDelete)

  const list = useInfinitePage<RoutingRuleView>({
    queryKey: [...routingKeys.list({ q }), pageSize],
    fetchPage: ({ after, signal }) => fetchRulePage({ q }, after, pageSize, signal),
  })
  const openCreate = () => patchSearch({ edit: 'new' })

  return (
    <>
      <PageHeader
        title={t('routing.title')}
        description={t('routing.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={openCreate}>
              <Plus aria-hidden="true" />
              {t('routing.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable<RoutingRuleView>
        tableId="routing-rules"
        label={t('routing.tableLabel')}
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
            icon={RouteIcon}
            title={t('routing.empty.title')}
            description={t('routing.empty.description')}
            action={
              <PrivilegeGate need="admin">
                <Button onClick={openCreate}>
                  <Plus aria-hidden="true" />
                  {t('routing.create')}
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
          kind="routing rule"
          name={toDelete.name}
          description={t('routing.delete.description')}
          requireTyping
          onConfirm={async () => {
            await deleteRule(toDelete.id, null)
            await queryClient.invalidateQueries({ queryKey: routingKeys.all })
            toast.success(t('routing.delete.done', { name: toDelete.name }))
          }}
        />
      ) : null}
    </>
  )
}

/** `/admin/routing-rules`: the list, or the editor of one rule while `?edit=` is set. */
export function RoutingPage() {
  const edit = useSearchParam('edit')
  return edit ? <RuleEditorView ruleId={edit} /> : <RuleList />
}
