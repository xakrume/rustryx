import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { UserPlus } from 'lucide-react'
import { useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { SourceBadge } from '@/features/roles/origin-badge'
import { rolePickerQuery } from '@/features/roles/api'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { userPage, type UserView } from './api'
import { UserCreateDialog } from './user-create-dialog'

const FILTER_IDS = ['q', 'source', 'enabled', 'role'] as const
const EDITOR_PARAMS = ['new'] as const

/**
 * Users of every source: filter by source, status and role, sort by name or creation time, open
 * one for its detail, create a local account in a dialog. Row state is text and icon, never color.
 */
export function UsersPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('users')
  const [creating, setCreating] = useSearchParam('new')
  const closeCreate = useClearSearchParams(EDITOR_PARAMS)
  const roles = useQuery(rolePickerQuery)

  const filters = useMemo(
    () => ({
      q: state.filters.q,
      source: state.filters.source,
      enabled: state.filters.enabled,
      role: state.filters.role,
    }),
    [state.filters.q, state.filters.source, state.filters.enabled, state.filters.role],
  )
  const list = useInfinitePage(userPage(filters, state.sort, prefs.pageSize))

  const columns = useMemo<AnyColumnDef<UserView>[]>(
    () => [
      {
        id: 'username',
        header: t('users.columns.username'),
        meta: { label: t('users.columns.username'), sortKey: 'username', hideable: false },
        cell: ({ row }) => (
          <Link
            to="/admin/users/$id"
            params={{ id: row.original.id }}
            className="font-mono font-medium text-link underline-offset-2 hover:underline"
          >
            {row.original.username}
          </Link>
        ),
      },
      {
        id: 'name',
        header: t('users.columns.name'),
        meta: { label: t('users.columns.name') },
        cell: ({ row }) => row.original.display_name ?? t('identity.noValue'),
      },
      {
        id: 'email',
        header: t('users.columns.email'),
        meta: { label: t('users.columns.email') },
        cell: ({ row }) => row.original.email ?? t('identity.noValue'),
      },
      {
        id: 'source',
        header: t('users.columns.source'),
        meta: { label: t('users.columns.source') },
        cell: ({ row }) => <SourceBadge source={row.original.source} />,
      },
      {
        id: 'status',
        header: t('users.columns.status'),
        meta: { label: t('users.columns.status') },
        cell: ({ row }) => (
          <StatusBadge
            status={row.original.enabled ? 'ok' : 'neutral'}
            label={row.original.enabled ? t('users.status.enabled') : t('users.status.disabled')}
          />
        ),
      },
      {
        id: 'roles',
        header: t('users.columns.roles'),
        meta: { label: t('users.columns.roles') },
        cell: ({ row }) => {
          const names = [...new Set(row.original.roles.map((r) => r.role.name))]
          return names.length === 0 ? (
            <span className="text-muted-foreground">{t('users.noRoles')}</span>
          ) : (
            <span
              className="inline-block max-w-48 truncate align-bottom font-mono"
              title={names.join(', ')}
            >
              {names.slice(0, 2).join(', ')}
              {names.length > 2 ? ` +${names.length - 2}` : ''}
            </span>
          )
        },
      },
      {
        id: 'lastLogin',
        header: t('users.columns.lastLogin'),
        meta: { label: t('users.columns.lastLogin') },
        cell: ({ row }) =>
          row.original.last_login_at === null ? (
            <span className="text-muted-foreground">{t('identity.never')}</span>
          ) : (
            <RelativeTime value={row.original.last_login_at} />
          ),
      },
      {
        id: 'created',
        header: t('users.columns.created'),
        meta: { label: t('users.columns.created'), sortKey: 'created_at' },
        cell: ({ row }) => <RelativeTime value={row.original.created_at} />,
      },
    ],
    [],
  )

  return (
    <>
      <PageHeader
        title={t('nav.items.users')}
        description={t('users.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={() => setCreating('1')}>
              <UserPlus aria-hidden="true" />
              {t('users.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable
        tableId="users"
        label={t('users.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(u) => u.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        sort={state.sort ?? 'username'}
        onSortChange={state.setSort}
        pageSize={prefs.pageSize}
        onPageSizeChange={prefs.setPageSize}
        hasActiveFilters={state.hasActiveFilters}
        onClearFilters={state.clearFilters}
        emptyState={
          <EmptyState title={t('users.empty.title')} description={t('users.empty.body')} />
        }
        toolbar={
          <FilterBar
            filters={[
              { id: 'q', type: 'search', label: t('users.filters.search') },
              {
                id: 'source',
                type: 'select',
                label: t('users.filters.source'),
                options: [
                  { value: 'local', label: t('identity.source.local') },
                  { value: 'ldap', label: t('identity.source.ldap') },
                  { value: 'oidc', label: t('identity.source.oidc') },
                ],
              },
              {
                id: 'enabled',
                type: 'select',
                label: t('users.filters.status'),
                options: [
                  { value: 'true', label: t('users.filters.enabledOnly') },
                  { value: 'false', label: t('users.filters.disabledOnly') },
                ],
              },
              {
                id: 'role',
                type: 'select',
                label: t('users.filters.role'),
                options: (roles.data ?? []).map((r) => ({ value: r.name, label: r.name })),
              },
            ]}
            values={state.filters}
            onChange={state.setFilter}
            onClear={state.clearFilters}
          />
        }
      />
      {creating !== undefined ? <UserCreateDialog onClose={closeCreate} /> : null}
    </>
  )
}
