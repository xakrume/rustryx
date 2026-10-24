import { X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { Button } from '@/components/ui/button'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { useClearSearchParams } from '@/features/roles/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { adminTokenPage, useRevokeAnyToken, type ApiTokenView } from './api'
import { tokenColumns } from './token-columns'

const FILTER_IDS = ['state', 'kind', 'never_used', 'expiring', 'owner_id', 'owner'] as const
const OWNER_PARAMS = ['owner_id', 'owner'] as const

/**
 * Every token of the system for administrators: filter by state, kind, usage, expiry and owner
 * (the owner filter arrives from the user page as `?owner_id=`), revoke after a confirmation that
 * names the token and its owner. Revoking is idempotent and takes effect on the next request on
 * every node; the owner is not notified.
 */
export function AdminTokensPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('admin-tokens')
  const [revoking, setRevoking] = useState<ApiTokenView | null>(null)
  const revoke = useRevokeAnyToken()
  const clearOwner = useClearSearchParams(OWNER_PARAMS)

  const filters = useMemo(
    () => ({
      state: state.filters.state,
      kind: state.filters.kind,
      never_used: state.filters.never_used,
      expiring: state.filters.expiring,
      owner_id: state.filters.owner_id,
    }),
    [
      state.filters.state,
      state.filters.kind,
      state.filters.never_used,
      state.filters.expiring,
      state.filters.owner_id,
    ],
  )
  const list = useInfinitePage(adminTokenPage(filters, prefs.pageSize))
  const columns = useMemo(
    () =>
      tokenColumns({
        admin: true,
        revokeLabel: (token) =>
          t('tokens.admin.revoke', { name: token.name, owner: token.owner.username }),
        onRevoke: setRevoking,
      }),
    [],
  )
  const ownerName = state.filters.owner ?? state.filters.owner_id

  return (
    <>
      <PageHeader title={t('nav.items.apiTokens')} description={t('tokens.admin.description')} />
      <DataTable
        tableId="admin-tokens"
        label={t('tokens.admin.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(token) => token.id}
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
            title={t('tokens.admin.empty.title')}
            description={t('tokens.admin.empty.body')}
          />
        }
        toolbar={
          <div className="space-y-2">
            <FilterBar
              filters={[
                {
                  id: 'state',
                  type: 'select',
                  label: t('tokens.admin.filters.state'),
                  options: (['active', 'expired', 'revoked', 'disabled'] as const).map((value) => ({
                    value,
                    label: t(`tokens.states.${value}`),
                  })),
                },
                {
                  id: 'kind',
                  type: 'select',
                  label: t('tokens.admin.filters.kind'),
                  options: (['personal', 'robot', 'service'] as const).map((value) => ({
                    value,
                    label: t(`tokens.kinds.${value}`),
                  })),
                },
                {
                  id: 'never_used',
                  type: 'select',
                  label: t('tokens.admin.filters.neverUsed'),
                  options: [{ value: 'true', label: t('tokens.admin.filters.neverUsedOnly') }],
                },
                {
                  id: 'expiring',
                  type: 'select',
                  label: t('tokens.admin.filters.expiring'),
                  options: [
                    { value: '7', label: t('tokens.admin.filters.expiring7') },
                    { value: '30', label: t('tokens.admin.filters.expiring30') },
                    { value: '90', label: t('tokens.admin.filters.expiring90') },
                  ],
                },
              ]}
              values={state.filters}
              onChange={state.setFilter}
              onClear={state.clearFilters}
            />
            {state.filters.owner_id ? (
              <p className="inline-flex items-center gap-1 rounded-md border border-border bg-muted py-0.5 pr-0.5 pl-2 text-xs">
                <span>{t('tokens.admin.ownerFilter', { name: ownerName ?? '' })}</span>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-6"
                  aria-label={t('tokens.admin.removeOwnerFilter')}
                  onClick={clearOwner}
                >
                  <X aria-hidden="true" />
                </Button>
              </p>
            ) : null}
          </div>
        }
      />
      <PrivilegeGate need="admin">
        {revoking ? (
          <ActionConfirmDialog
            open
            onOpenChange={(open) => !open && setRevoking(null)}
            title={t('tokens.mine.revokeTitle', { name: revoking.name })}
            variant="destructive"
            confirmLabel={t('tokens.mine.revokeConfirm')}
            description={t('tokens.admin.revokeBody', {
              name: revoking.name,
              owner: revoking.owner.username,
            })}
            onConfirm={async () => {
              await revoke.mutateAsync(revoking.id)
              toast.success(
                t('tokens.admin.revokeDone', {
                  name: revoking.name,
                  owner: revoking.owner.username,
                }),
              )
            }}
          />
        ) : null}
      </PrivilegeGate>
    </>
  )
}
