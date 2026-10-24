import { Eye, Lock, Pencil, Plus, Trash2, TriangleAlert } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { privilegePage, type PrivilegeView } from './api'
import { PrivilegeDeleteDialog } from './privilege-delete-dialog'
import { PrivilegeEditor } from './privilege-editor'

const FILTER_IDS = ['q', 'kind', 'repository', 'builtin'] as const
const EDITOR_PARAMS = ['edit', 'new'] as const

function RepositoryCell({ privilege }: { privilege: PrivilegeView }) {
  const { grant } = privilege
  if (grant.type === 'admin')
    return <span className="text-muted-foreground">{t('identity.noValue')}</span>
  const format = grant.type === 'repository-view' ? grant.format : null
  return (
    <span className="inline-flex items-center gap-2">
      {grant.repository === '*' ? (
        <Badge variant="warning" title={t('privileges.editor.wildcardWarning')}>
          <TriangleAlert aria-hidden="true" />
          {t('identity.allRepositories')}
        </Badge>
      ) : (
        <span className="font-mono">{grant.repository}</span>
      )}
      {format !== null ? (
        <span className="font-mono text-xs text-muted-foreground">{format}</span>
      ) : null}
    </span>
  )
}

function ActionBadges({ privilege }: { privilege: PrivilegeView }) {
  return (
    <span className="inline-flex gap-1">
      {privilege.actions.map((action) => (
        <Badge key={action}>{t(`identity.action.${action}`)}</Badge>
      ))}
    </span>
  )
}

/**
 * Privileges: what each one allows (kind, repository, format, actions, selector) and which roles
 * use it. The editor dialog lives in the address bar like the roles one. A wildcard repository is
 * marked with a warning badge in the list and a visible warning in the editor.
 */
export function PrivilegesPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('privileges')
  const [editId, setEditId] = useSearchParam('edit')
  const [creating, setCreating] = useSearchParam('new')
  const closeEditor = useClearSearchParams(EDITOR_PARAMS)
  const [deleting, setDeleting] = useState<PrivilegeView | null>(null)

  const filters = useMemo(
    () => ({
      q: state.filters.q,
      kind: state.filters.kind,
      repository: state.filters.repository,
      builtin: state.filters.builtin,
    }),
    [state.filters.q, state.filters.kind, state.filters.repository, state.filters.builtin],
  )
  const list = useInfinitePage(privilegePage(filters, prefs.pageSize))

  const columns = useMemo<AnyColumnDef<PrivilegeView>[]>(
    () => [
      {
        id: 'name',
        header: t('privileges.columns.name'),
        meta: { label: t('privileges.columns.name'), hideable: false },
        cell: ({ row }) => (
          <span className="inline-flex items-center gap-2">
            <button
              type="button"
              onClick={() => setEditId(row.original.id)}
              className="focus-inset rounded-sm font-mono font-medium text-link underline-offset-2 hover:underline"
            >
              {row.original.name}
            </button>
            {row.original.builtin ? (
              <Badge>
                <Lock aria-hidden="true" />
                {t('identity.builtin')}
              </Badge>
            ) : null}
          </span>
        ),
      },
      {
        id: 'kind',
        header: t('privileges.columns.kind'),
        meta: { label: t('privileges.columns.kind') },
        cell: ({ row }) => t(`identity.grantKind.${row.original.grant.type}`),
      },
      {
        id: 'repository',
        header: t('privileges.columns.repository'),
        meta: { label: t('privileges.columns.repository') },
        cell: ({ row }) => <RepositoryCell privilege={row.original} />,
      },
      {
        id: 'actions',
        header: t('privileges.columns.actions'),
        meta: { label: t('privileges.columns.actions') },
        cell: ({ row }) => <ActionBadges privilege={row.original} />,
      },
      {
        id: 'selector',
        header: t('privileges.columns.selector'),
        meta: { label: t('privileges.columns.selector') },
        cell: ({ row }) =>
          row.original.selector ? (
            <span className="font-mono">{row.original.selector.name}</span>
          ) : (
            <span className="text-muted-foreground">{t('identity.noValue')}</span>
          ),
      },
      {
        id: 'roles',
        header: t('privileges.columns.roles'),
        meta: { label: t('privileges.columns.roles') },
        cell: ({ row }) => {
          const names = row.original.roles.map((r) => r.name)
          return names.length === 0 ? (
            <span className="text-muted-foreground">{t('privileges.noRoles')}</span>
          ) : (
            <span
              className="inline-block max-w-40 truncate align-bottom font-mono"
              title={names.join(', ')}
            >
              {names.slice(0, 2).join(', ')}
              {names.length > 2 ? ` +${names.length - 2}` : ''}
            </span>
          )
        },
      },
      {
        id: 'rowActions',
        header: () => <span className="sr-only">{t('privileges.columns.rowActions')}</span>,
        meta: { label: t('privileges.columns.rowActions'), hideable: false, align: 'right' },
        cell: ({ row }) => {
          const p = row.original
          return (
            <span className="inline-flex gap-1">
              <Button
                variant="ghost"
                size="icon"
                aria-label={t(
                  p.builtin ? 'privileges.actionLabels.view' : 'privileges.actionLabels.edit',
                  {
                    name: p.name,
                  },
                )}
                onClick={() => setEditId(p.id)}
              >
                {p.builtin ? <Eye aria-hidden="true" /> : <Pencil aria-hidden="true" />}
              </Button>
              <PrivilegeGate need="admin">
                <Button
                  variant="ghost"
                  size="icon"
                  disabled={p.builtin}
                  aria-label={t(
                    p.builtin
                      ? 'privileges.actionLabels.deleteBuiltin'
                      : 'privileges.actionLabels.delete',
                    { name: p.name },
                  )}
                  onClick={() => setDeleting(p)}
                >
                  <Trash2 aria-hidden="true" />
                </Button>
              </PrivilegeGate>
            </span>
          )
        },
      },
    ],
    [setEditId],
  )

  return (
    <>
      <PageHeader
        title={t('nav.items.privileges')}
        description={t('privileges.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={() => setCreating('1')}>
              <Plus aria-hidden="true" />
              {t('privileges.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable
        tableId="privileges"
        label={t('privileges.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(p) => p.id}
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
          <EmptyState
            title={t('privileges.empty.title')}
            description={t('privileges.empty.body')}
          />
        }
        toolbar={
          <FilterBar
            filters={[
              { id: 'q', type: 'search', label: t('privileges.filters.search') },
              { id: 'repository', type: 'search', label: t('privileges.filters.repository') },
              {
                id: 'kind',
                type: 'select',
                label: t('privileges.filters.kind'),
                options: [
                  { value: 'repository-view', label: t('identity.grantKind.repository-view') },
                  {
                    value: 'repository-content-selector',
                    label: t('identity.grantKind.repository-content-selector'),
                  },
                  { value: 'admin', label: t('identity.grantKind.admin') },
                ],
              },
              {
                id: 'builtin',
                type: 'select',
                label: t('privileges.filters.builtin'),
                options: [
                  { value: 'true', label: t('privileges.filters.builtinOnly') },
                  { value: 'false', label: t('privileges.filters.customOnly') },
                ],
              },
            ]}
            values={state.filters}
            onChange={state.setFilter}
            onClear={state.clearFilters}
          />
        }
      />
      {creating !== undefined || editId !== undefined ? (
        <PrivilegeEditor privilegeId={editId ?? null} onClose={closeEditor} />
      ) : null}
      <PrivilegeDeleteDialog privilege={deleting} onClose={() => setDeleting(null)} />
    </>
  )
}
