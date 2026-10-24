import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { DataTable, FilterBar, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { REPO_FORMATS } from '@/features/privileges/api'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  anonymousQuery,
  repositoryPage,
  useSetRepositoryFlags,
  type FlagChange,
  type RepositoryView,
} from './api'
import { GlobalSwitchCard } from './global-switch-card'

const FILTER_IDS = ['q', 'format', 'allow_anonymous'] as const
const SHOWN_NAMES = 5

function effectiveStatus(repository: RepositoryView, globalOn: boolean | undefined) {
  if (repository.anonymous_effective) {
    return <StatusBadge status="warning" label={t('anonymous.repositories.effective.public')} />
  }
  if (repository.allow_anonymous && globalOn === false) {
    return <StatusBadge status="info" label={t('anonymous.repositories.effective.globalOff')} />
  }
  return <StatusBadge status="neutral" label={t('anonymous.repositories.effective.private')} />
}

/**
 * Anonymous access: the global switch (read only in this version, with the reason) and every
 * repository's flag with the effective result next to it. A flag turned ON, alone or in bulk,
 * first asks and names the repositories it exposes; turning flags off needs no question.
 */
export function AnonymousPage() {
  const state = useUrlTableState(FILTER_IDS)
  const prefs = useTablePrefs('anonymous-repositories')
  const setFlags = useSetRepositoryFlags()
  const [exposing, setExposing] = useState<FlagChange[] | null>(null)
  const filters = useMemo(
    () => ({
      q: state.filters.q,
      format: state.filters.format,
      allow_anonymous: state.filters.allow_anonymous,
    }),
    [state.filters.q, state.filters.format, state.filters.allow_anonymous],
  )
  const list = useInfinitePage(repositoryPage(filters, prefs.pageSize))
  // Shares the query of the global switch card: tells "flag on, but the switch is off".
  const globalOn = useQuery(anonymousQuery).data?.data.global_enabled

  async function apply(changes: FlagChange[]): Promise<void> {
    try {
      await setFlags.mutateAsync(changes)
      const allowed = changes.filter((c) => c.allow_anonymous).length
      toast.success(
        allowed > 0
          ? t('anonymous.changed.allowed', { count: allowed })
          : t('anonymous.changed.disallowed', { count: changes.length }),
      )
    } catch (error) {
      toast.apiError(error, t('anonymous.changed.failed'))
      throw error
    }
  }

  /** Allowing asks first; disallowing is applied at once. */
  function request(changes: FlagChange[]): void {
    if (changes.some((c) => c.allow_anonymous)) setExposing(changes)
    else void apply(changes).catch(() => undefined)
  }

  const columns = useMemo<AnyColumnDef<RepositoryView>[]>(
    () => [
      {
        id: 'name',
        header: t('anonymous.repositories.columns.name'),
        meta: { label: t('anonymous.repositories.columns.name'), hideable: false },
        cell: ({ row }) => <span className="font-mono font-medium">{row.original.name}</span>,
      },
      {
        id: 'format',
        header: t('anonymous.repositories.columns.format'),
        meta: { label: t('anonymous.repositories.columns.format') },
        cell: ({ row }) => <span className="font-mono">{row.original.format}</span>,
      },
      {
        id: 'type',
        header: t('anonymous.repositories.columns.type'),
        meta: { label: t('anonymous.repositories.columns.type') },
        cell: ({ row }) => row.original.repo_type,
      },
      {
        id: 'flag',
        header: t('anonymous.repositories.columns.flag'),
        meta: { label: t('anonymous.repositories.columns.flag'), hideable: false },
        cell: ({ row }) => (
          <Switch
            checked={row.original.allow_anonymous}
            disabled={setFlags.isPending}
            aria-label={t('anonymous.repositories.flagLabel', { name: row.original.name })}
            onCheckedChange={(checked) =>
              request([{ name: row.original.name, allow_anonymous: checked }])
            }
          />
        ),
      },
      {
        id: 'effective',
        header: t('anonymous.repositories.columns.effective'),
        meta: { label: t('anonymous.repositories.columns.effective') },
        cell: ({ row }) => effectiveStatus(row.original, globalOn),
      },
    ],
    // `request` closes over setFlags only through stable callbacks; the list items drive the rows.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [setFlags.isPending, globalOn],
  )

  const names = exposing?.filter((c) => c.allow_anonymous).map((c) => c.name) ?? []
  const shown =
    names.length > SHOWN_NAMES
      ? t('anonymous.expose.more', {
          names: names.slice(0, SHOWN_NAMES).join(', '),
          more: names.length - SHOWN_NAMES,
        })
      : names.join(', ')

  return (
    <>
      <PageHeader title={t('nav.items.anonymousAccess')} description={t('anonymous.description')} />
      <div className="space-y-4">
        <GlobalSwitchCard />
        <DataTable
          tableId="anonymous-repositories"
          label={t('anonymous.repositories.tableLabel')}
          columns={columns}
          rows={list.items}
          getRowId={(r) => r.name}
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
          selectable
          selectionLabel={(r) => t('anonymous.repositories.selectionLabel', { name: r.name })}
          resetSelectionKey={JSON.stringify(filters)}
          bulkActions={({ selectedRows, clearSelection }) => (
            <>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  request(selectedRows.map((r) => ({ name: r.name, allow_anonymous: true })))
                  clearSelection()
                }}
              >
                {t('anonymous.repositories.bulkAllow')}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  request(selectedRows.map((r) => ({ name: r.name, allow_anonymous: false })))
                  clearSelection()
                }}
              >
                {t('anonymous.repositories.bulkDisallow')}
              </Button>
            </>
          )}
          emptyState={
            <EmptyState
              title={t('anonymous.repositories.empty.title')}
              description={t('anonymous.repositories.empty.body')}
            />
          }
          toolbar={
            <FilterBar
              filters={[
                { id: 'q', type: 'search', label: t('anonymous.repositories.filters.search') },
                {
                  id: 'format',
                  type: 'select',
                  label: t('anonymous.repositories.filters.format'),
                  options: REPO_FORMATS.map((f) => ({ value: f, label: f })),
                },
                {
                  id: 'allow_anonymous',
                  type: 'select',
                  label: t('anonymous.repositories.filters.flag'),
                  options: [
                    { value: 'true', label: t('anonymous.repositories.filters.flagOn') },
                    { value: 'false', label: t('anonymous.repositories.filters.flagOff') },
                  ],
                },
              ]}
              values={state.filters}
              onChange={state.setFilter}
              onClear={state.clearFilters}
            />
          }
        />
      </div>
      <ActionConfirmDialog
        open={exposing !== null}
        onOpenChange={(open) => !open && setExposing(null)}
        title={t('anonymous.expose.title')}
        description={t('anonymous.expose.body', { count: names.length, names: shown })}
        confirmLabel={t('anonymous.expose.confirm')}
        variant="destructive"
        onConfirm={async () => {
          if (exposing !== null) await apply(exposing)
        }}
      />
    </>
  )
}
