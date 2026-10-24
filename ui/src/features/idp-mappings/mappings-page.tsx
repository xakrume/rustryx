import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Pencil, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { CopyButton } from '@/components/copy-button'
import { DataTable, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useClearSearchParams, useSearchParam } from '@/features/roles/search-params'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  mappingKeys,
  mappingPage,
  updateMapping,
  useDeleteMapping,
  type MappingView,
  type Provider,
} from './api'
import { AdminGroupsNote } from './admin-groups-note'
import { MappingDialog } from './mapping-dialog'
import { MappingTester } from './mapping-tester'
import { priorityChanges } from './reorder'

const EDITOR_PARAMS = ['edit', 'new'] as const
const PROVIDERS: readonly Provider[] = ['oidc', 'ldap']

/**
 * Group mappings: one tab per identity provider, each with the admin groups note, the mappings in
 * evaluation order (lower priority first) with move up and move down buttons that work from the
 * keyboard, an add and edit dialog and a tester for a list of groups.
 */
export function MappingsPage() {
  const [providerParam, setProvider] = useSearchParam('provider')
  const provider: Provider = providerParam === 'ldap' ? 'ldap' : 'oidc'
  return (
    <>
      <PageHeader title={t('nav.items.groupMappings')} description={t('mappings.description')} />
      <Tabs
        value={provider}
        onValueChange={(value) => setProvider(value === 'ldap' ? 'ldap' : undefined)}
      >
        <TabsList aria-label={t('mappings.tabs.label')}>
          {PROVIDERS.map((p) => (
            <TabsTrigger key={p} value={p}>
              {t(`mappings.tabs.${p}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        {PROVIDERS.map((p) => (
          <TabsContent key={p} value={p}>
            <ProviderPanel provider={p} />
          </TabsContent>
        ))}
      </Tabs>
    </>
  )
}

function ProviderPanel({ provider }: { provider: Provider }) {
  const queryClient = useQueryClient()
  const prefs = useTablePrefs(`mappings-${provider}`)
  const [editId, setEditId] = useSearchParam('edit')
  const [creating, setCreating] = useSearchParam('new')
  const closeEditor = useClearSearchParams(EDITOR_PARAMS)
  const [deleting, setDeleting] = useState<MappingView | null>(null)
  const [announcement, setAnnouncement] = useState('')
  const remove = useDeleteMapping()
  const list = useInfinitePage(mappingPage(provider, prefs.pageSize))
  const editing = list.items.find((m) => m.id === editId)

  const move = useMutation({
    meta: { silent: true },
    mutationFn: async ({ index, direction }: { index: number; direction: -1 | 1 }) => {
      const changes = priorityChanges(list.items, index, direction)
      for (const change of changes) {
        const row = list.items.find((m) => m.id === change.id)
        if (row) await updateMapping(row.id, { role_id: row.role_id, priority: change.priority })
      }
      return { index, direction }
    },
    onSuccess: ({ index, direction }) => {
      const group = list.items[index]?.external_group ?? ''
      setAnnouncement(
        t('mappings.moved', { group, position: index + direction + 1, count: list.items.length }),
      )
    },
    onError: (error) => toast.apiError(error, t('mappings.moveFailed')),
    onSettled: () => queryClient.invalidateQueries({ queryKey: mappingKeys.all }),
  })

  const columns = useMemo<AnyColumnDef<MappingView>[]>(
    () => [
      {
        id: 'order',
        header: t('mappings.columns.order'),
        meta: { label: t('mappings.columns.order'), hideable: false },
        cell: ({ row }) => {
          const index = row.index
          const group = row.original.external_group
          return (
            <span className="inline-flex items-center gap-1">
              <span className="w-10 text-right tabular-nums">{row.original.priority}</span>
              <PrivilegeGate need="admin">
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={t('mappings.moveUp', { group })}
                  disabled={index === 0 || move.isPending}
                  onClick={() => move.mutate({ index, direction: -1 })}
                >
                  <ArrowUp aria-hidden="true" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={t('mappings.moveDown', { group })}
                  disabled={index === list.items.length - 1 || move.isPending}
                  onClick={() => move.mutate({ index, direction: 1 })}
                >
                  <ArrowDown aria-hidden="true" />
                </Button>
              </PrivilegeGate>
            </span>
          )
        },
      },
      {
        id: 'group',
        header: t('mappings.columns.group'),
        meta: {
          label: t('mappings.columns.group'),
          hideable: false,
          className: 'whitespace-normal',
        },
        cell: ({ row }) => (
          <span className="inline-flex items-center gap-1">
            <code className="font-mono text-xs break-all">{row.original.external_group}</code>
            <CopyButton
              value={row.original.external_group}
              what={t('mappings.columns.group')}
              size="icon"
              className="size-6"
            />
          </span>
        ),
      },
      {
        id: 'role',
        header: t('mappings.columns.role'),
        meta: { label: t('mappings.columns.role') },
        cell: ({ row }) => <span className="font-mono">{row.original.role.name}</span>,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">{t('mappings.columns.actions')}</span>,
        meta: { label: t('mappings.columns.actions'), hideable: false, align: 'right' },
        cell: ({ row }) => (
          <PrivilegeGate need="admin">
            <span className="inline-flex gap-1">
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('mappings.edit', { group: row.original.external_group })}
                onClick={() => setEditId(row.original.id)}
              >
                <Pencil aria-hidden="true" />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('mappings.delete', { group: row.original.external_group })}
                onClick={() => setDeleting(row.original)}
              >
                <Trash2 aria-hidden="true" />
              </Button>
            </span>
          </PrivilegeGate>
        ),
      },
    ],
    [list.items.length, move, setEditId],
  )

  return (
    <div className="space-y-4">
      <AdminGroupsNote provider={provider} />
      <div className="flex justify-end">
        <PrivilegeGate need="admin">
          <Button onClick={() => setCreating('1')}>
            <Plus aria-hidden="true" />
            {t('mappings.create')}
          </Button>
        </PrivilegeGate>
      </div>
      <DataTable
        tableId={`mappings-${provider}`}
        label={t('mappings.tableLabel')}
        columns={columns}
        rows={list.items}
        getRowId={(m) => m.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        pageSize={prefs.pageSize}
        onPageSizeChange={prefs.setPageSize}
        emptyState={
          <EmptyState title={t('mappings.empty.title')} description={t('mappings.empty.body')} />
        }
      />
      <p role="status" className="sr-only">
        {announcement}
      </p>
      <MappingTester provider={provider} />
      {creating !== undefined ? (
        <MappingDialog provider={provider} mapping={null} onClose={closeEditor} />
      ) : null}
      {editing ? (
        <MappingDialog provider={provider} mapping={editing} onClose={closeEditor} />
      ) : null}
      {deleting ? (
        <ConfirmDialog
          open
          onOpenChange={(open) => !open && setDeleting(null)}
          kind={t('mappings.deleteDialog.kind')}
          name={deleting.external_group}
          confirmLabel={t('mappings.deleteDialog.confirm')}
          description={t('mappings.deleteDialog.description', {
            group: deleting.external_group,
            role: deleting.role.name,
          })}
          onConfirm={async () => {
            await remove.mutateAsync(deleting.id)
            toast.success(t('mappings.deleteDialog.deleted', { group: deleting.external_group }))
          }}
        />
      ) : null}
    </div>
  )
}
