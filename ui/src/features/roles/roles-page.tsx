import { Eye, Lock, Pencil, Plus, Trash2 } from 'lucide-react'
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
import { rolePage, type RoleView } from './api'
import { RoleDeleteDialog } from './role-delete-dialog'
import { RoleEditor } from './role-editor'
import { useClearSearchParams, useSearchParam } from './search-params'

const FILTER_IDS = ['q', 'builtin'] as const
const EDITOR_PARAMS = ['edit', 'new'] as const

/** `a, b, c +2`: the first names of a list, the rest as a count (the full list is in the text). */
function NameList({ names, none }: { names: readonly string[]; none: string }) {
  if (names.length === 0) return <span className="text-muted-foreground">{none}</span>
  const shown = names.slice(0, 2).join(', ')
  return (
    <span
      className="inline-block max-w-48 truncate align-bottom font-mono"
      title={names.join(', ')}
    >
      {shown}
      {names.length > 2 ? ` +${names.length - 2}` : ''}
    </span>
  )
}

function RoleName({ role, onOpen }: { role: RoleView; onOpen: () => void }) {
  return (
    <span className="inline-flex items-center gap-2">
      <button
        type="button"
        onClick={onOpen}
        className="focus-inset rounded-sm font-mono font-medium text-link underline-offset-2 hover:underline"
      >
        {role.name}
      </button>
      {role.builtin ? (
        <Badge>
          <Lock aria-hidden="true" />
          {t('identity.builtin')}
        </Badge>
      ) : null}
    </span>
  )
}

/**
 * Roles: the list with nesting, privileges and usage, an editor dialog that lives in the address
 * bar (`?edit=<id>`, `?new=1`) so a view can be shared and Back closes it, and the delete flow
 * that names the role and what depends on it. Built-in roles open read only.
 */
export function RolesPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('roles')
  const [editId, setEditId] = useSearchParam('edit')
  const [creating, setCreating] = useSearchParam('new')
  const closeEditor = useClearSearchParams(EDITOR_PARAMS)
  const [deleting, setDeleting] = useState<RoleView | null>(null)

  const filters = useMemo(
    () => ({ q: state.filters.q, builtin: state.filters.builtin }),
    [state.filters.q, state.filters.builtin],
  )
  const list = useInfinitePage(rolePage(filters, prefs.pageSize))

  const columns = useMemo<AnyColumnDef<RoleView>[]>(
    () => [
      {
        id: 'name',
        header: t('roles.columns.name'),
        meta: { label: t('roles.columns.name'), hideable: false },
        cell: ({ row }) => (
          <RoleName role={row.original} onOpen={() => setEditId(row.original.id)} />
        ),
      },
      {
        id: 'description',
        header: t('roles.columns.description'),
        meta: { label: t('roles.columns.description') },
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
        id: 'privileges',
        header: t('roles.columns.privileges'),
        meta: { label: t('roles.columns.privileges') },
        cell: ({ row }) => (
          <NameList
            names={row.original.privileges.map((p) => p.name)}
            none={t('roles.noneDirect')}
          />
        ),
      },
      {
        id: 'nested',
        header: t('roles.columns.nested'),
        meta: { label: t('roles.columns.nested') },
        cell: ({ row }) => (
          <NameList
            names={row.original.nested_roles.map((r) => r.name)}
            none={t('roles.noNested')}
          />
        ),
      },
      {
        id: 'users',
        header: t('roles.columns.users'),
        meta: { label: t('roles.columns.users'), align: 'right' },
        cell: ({ row }) => row.original.assigned_users,
      },
      {
        id: 'groups',
        header: t('roles.columns.groups'),
        meta: { label: t('roles.columns.groups'), align: 'right' },
        cell: ({ row }) => row.original.mapped_groups,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">{t('roles.columns.actions')}</span>,
        meta: { label: t('roles.columns.actions'), hideable: false, align: 'right' },
        cell: ({ row }) => {
          const role = row.original
          return (
            <span className="inline-flex gap-1">
              <Button
                variant="ghost"
                size="icon"
                aria-label={t(role.builtin ? 'roles.actions.view' : 'roles.actions.edit', {
                  name: role.name,
                })}
                onClick={() => setEditId(role.id)}
              >
                {role.builtin ? <Eye aria-hidden="true" /> : <Pencil aria-hidden="true" />}
              </Button>
              <PrivilegeGate need="admin">
                <Button
                  variant="ghost"
                  size="icon"
                  disabled={role.builtin}
                  aria-label={t(
                    role.builtin ? 'roles.actions.deleteBuiltin' : 'roles.actions.delete',
                    {
                      name: role.name,
                    },
                  )}
                  onClick={() => setDeleting(role)}
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
        title={t('nav.items.roles')}
        description={t('roles.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={() => setCreating('1')}>
              <Plus aria-hidden="true" />
              {t('roles.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable
        tableId="roles"
        label={t('roles.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(role) => role.id}
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
          <EmptyState title={t('roles.empty.title')} description={t('roles.empty.body')} />
        }
        toolbar={
          <FilterBar
            filters={[
              { id: 'q', type: 'search', label: t('roles.filters.search') },
              {
                id: 'builtin',
                type: 'select',
                label: t('roles.filters.builtin'),
                options: [
                  { value: 'true', label: t('roles.filters.builtinOnly') },
                  { value: 'false', label: t('roles.filters.customOnly') },
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
        <RoleEditor roleId={editId ?? null} onClose={closeEditor} />
      ) : null}
      <RoleDeleteDialog role={deleting} onClose={() => setDeleting(null)} />
    </>
  )
}
