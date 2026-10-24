import { Plus } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { SecretOnce } from '@/components/secret-once'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useMe } from '@/features/auth/use-permissions'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { myTokenPage, useRevokeMyToken, type ApiTokenView } from './api'
import { CreateTokenDialog } from './create-token-dialog'
import { tokenColumns } from './token-columns'
import { TokenUsageHints } from './token-usage-hints'

const FILTER_IDS = ['state'] as const
const EDITOR_PARAMS = ['new'] as const

/**
 * Personal tokens: create (the secret is shown once), list with last use, revoke after a
 * confirmation that names the token. The secret lives in this component's state only until the
 * person ticks that they stored it; it is in no cache and no address.
 */
export function MyTokensPage() {
  const me = useMe()
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('my-tokens')
  const [creating, setCreating] = useSearchParam('new')
  const closeCreate = useClearSearchParams(EDITOR_PARAMS)
  const [secret, setSecret] = useState<string | null>(null)
  const [revoking, setRevoking] = useState<ApiTokenView | null>(null)
  const revoke = useRevokeMyToken()

  const filters = useMemo(() => ({ state: state.filters.state }), [state.filters.state])
  const list = useInfinitePage(myTokenPage(filters, prefs.pageSize))
  const columns = useMemo(
    () =>
      tokenColumns({
        admin: false,
        revokeLabel: (token) => t('tokens.mine.revoke', { name: token.name }),
        onRevoke: setRevoking,
      }),
    [],
  )
  const policy = me?.token_policy

  return (
    <>
      <PageHeader
        title={t('nav.items.personalTokens')}
        description={t('tokens.mine.description')}
        actions={
          <Button onClick={() => setCreating('1')}>
            <Plus aria-hidden="true" />
            {t('tokens.mine.create')}
          </Button>
        }
      />
      {policy ? (
        <Alert className="mb-4">
          {t('tokens.mine.policy', { days: policy.default_ttl_days, max: policy.max_ttl_days })}
        </Alert>
      ) : null}
      <div className="space-y-4">
        <DataTable
          tableId="my-tokens"
          label={t('tokens.mine.tableLabel')}
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
              title={t('tokens.mine.empty.title')}
              description={t('tokens.mine.empty.body')}
              action={<Button onClick={() => setCreating('1')}>{t('tokens.mine.create')}</Button>}
            />
          }
          toolbar={
            <FilterBar
              filters={[
                {
                  id: 'state',
                  type: 'select',
                  label: t('tokens.mine.filterState'),
                  options: (['active', 'expired', 'revoked'] as const).map((value) => ({
                    value,
                    label: t(`tokens.states.${value}`),
                  })),
                },
              ]}
              values={state.filters}
              onChange={state.setFilter}
              onClear={state.clearFilters}
            />
          }
        />
        <TokenUsageHints />
      </div>
      {creating !== undefined ? (
        <CreateTokenDialog
          onCreated={(created) => setSecret(created.secret)}
          onClose={closeCreate}
        />
      ) : null}
      <SecretOnce secret={secret} what={t('tokens.secretWhat')} onDone={() => setSecret(null)} />
      {revoking ? (
        <ActionConfirmDialog
          open
          onOpenChange={(open) => !open && setRevoking(null)}
          title={t('tokens.mine.revokeTitle', { name: revoking.name })}
          variant="destructive"
          confirmLabel={t('tokens.mine.revokeConfirm')}
          description={t('tokens.mine.revokeBody', {
            name: revoking.name,
            prefix: revoking.display_prefix,
          })}
          onConfirm={async () => {
            await revoke.mutateAsync(revoking.id)
            toast.success(t('tokens.mine.revokeDone', { name: revoking.name }))
          }}
        />
      ) : null}
    </>
  )
}
