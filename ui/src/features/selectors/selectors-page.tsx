import { Pencil, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { RelativeTime } from '@/components/relative-time'
import { Button } from '@/components/ui/button'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { selectorPage, type SelectorView } from './api'
import { SelectorDeleteDialog } from './selector-delete-dialog'
import { SelectorEditor } from './selector-editor'

const FILTER_IDS = ['q'] as const
const EDITOR_PARAMS = ['edit', 'new'] as const

/**
 * Content selectors: the list with the expression and the privileges that use each one, the
 * editor dialog in the address bar (`?edit=<id>`, `?new=1`), and delete. A selector in use cannot
 * be deleted (the API has no force for it), so its delete dialog explains and lists the blockers.
 */
export function SelectorsPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('content-selectors')
  const [editId, setEditId] = useSearchParam('edit')
  const [creating, setCreating] = useSearchParam('new')
  const closeEditor = useClearSearchParams(EDITOR_PARAMS)
  const [deleting, setDeleting] = useState<SelectorView | null>(null)

  const filters = useMemo(() => ({ q: state.filters.q }), [state.filters.q])
  const list = useInfinitePage(selectorPage(filters, prefs.pageSize))

  const columns = useMemo<AnyColumnDef<SelectorView>[]>(
    () => [
      {
        id: 'name',
        header: t('selectors.columns.name'),
        meta: { label: t('selectors.columns.name'), hideable: false },
        cell: ({ row }) => (
          <button
            type="button"
            onClick={() => setEditId(row.original.id)}
            className="focus-inset rounded-sm font-mono font-medium text-link underline-offset-2 hover:underline"
          >
            {row.original.name}
          </button>
        ),
      },
      {
        id: 'description',
        header: t('selectors.columns.description'),
        meta: { label: t('selectors.columns.description') },
        cell: ({ row }) => (
          <span
            className="inline-block max-w-56 truncate align-bottom"
            title={row.original.description}
          >
            {row.original.description || t('identity.noValue')}
          </span>
        ),
      },
      {
        id: 'expression',
        header: t('selectors.columns.expression'),
        meta: { label: t('selectors.columns.expression') },
        cell: ({ row }) => (
          <code
            className="inline-block max-w-80 truncate align-bottom font-mono text-xs"
            title={row.original.expression}
          >
            {row.original.expression}
          </code>
        ),
      },
      {
        id: 'usedBy',
        header: t('selectors.columns.usedBy'),
        meta: { label: t('selectors.columns.usedBy') },
        cell: ({ row }) => {
          const names = row.original.used_by_privileges.map((p) => p.name)
          return names.length === 0 ? (
            <span className="text-muted-foreground">{t('selectors.unused')}</span>
          ) : (
            <span
              className="inline-block max-w-48 truncate align-bottom font-mono"
              title={names.join(', ')}
            >
              {names.join(', ')}
            </span>
          )
        },
      },
      {
        id: 'updated',
        header: t('selectors.columns.updated'),
        meta: { label: t('selectors.columns.updated') },
        cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">{t('selectors.columns.actions')}</span>,
        meta: { label: t('selectors.columns.actions'), hideable: false, align: 'right' },
        cell: ({ row }) => (
          <span className="inline-flex gap-1">
            <Button
              variant="ghost"
              size="icon"
              aria-label={t('selectors.actionLabels.edit', { name: row.original.name })}
              onClick={() => setEditId(row.original.id)}
            >
              <Pencil aria-hidden="true" />
            </Button>
            <PrivilegeGate need="admin">
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('selectors.actionLabels.delete', { name: row.original.name })}
                onClick={() => setDeleting(row.original)}
              >
                <Trash2 aria-hidden="true" />
              </Button>
            </PrivilegeGate>
          </span>
        ),
      },
    ],
    [setEditId],
  )

  return (
    <>
      <PageHeader
        title={t('nav.items.contentSelectors')}
        description={t('selectors.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={() => setCreating('1')}>
              <Plus aria-hidden="true" />
              {t('selectors.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable
        tableId="content-selectors"
        label={t('selectors.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(s) => s.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        pageSize={prefs.pageSize}
        onPageSizeChange={prefs.setPageSize}
        hasActiveFilters={state.hasActiveFilters}
        onClearFilters={state.clearFilters}
        emptyState={
          <EmptyState title={t('selectors.empty.title')} description={t('selectors.empty.body')} />
        }
        toolbar={
          <FilterBar
            filters={[{ id: 'q', type: 'search', label: t('selectors.filters.search') }]}
            values={state.filters}
            onChange={state.setFilter}
            onClear={state.clearFilters}
          />
        }
      />
      {creating !== undefined || editId !== undefined ? (
        <SelectorEditor selectorId={editId ?? null} onClose={closeEditor} />
      ) : null}
      <SelectorDeleteDialog selector={deleting} onClose={() => setDeleting(null)} />
    </>
  )
}
