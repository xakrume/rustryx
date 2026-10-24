import { Link } from '@tanstack/react-router'
import { HardDrive, Plus } from 'lucide-react'
import { useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { BytesText } from '@/components/bytes-text'
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useSearchParam, useSearchPatch } from '@/features/system/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { blobStoreKeys, fetchBlobStorePage, type BlobStoreView } from './api'
import { BlobStoreCreateView } from './blob-store-create'
import { StorageTrendWidget } from '@/features/system-dashboard/storage-widget'
import { HealthBadge, QuotaBadge, UsageBar } from './usage-bar'
import { usageDisplay } from './form-model'

const FILTER_IDS = ['q', 'kind'] as const

const FILTERS: readonly FilterDef[] = [
  {
    id: 'q',
    type: 'search',
    label: t('blobStores.filters.name'),
    placeholder: t('blobStores.filters.namePlaceholder'),
  },
  {
    id: 'kind',
    type: 'select',
    label: t('blobStores.filters.kind'),
    options: [
      { value: 'local', label: t('blobStores.kind.local') },
      { value: 's3', label: t('blobStores.kind.s3') },
    ],
  },
]

function useColumns(): AnyColumnDef<BlobStoreView>[] {
  return useMemo<AnyColumnDef<BlobStoreView>[]>(
    () => [
      {
        accessorKey: 'name',
        header: t('blobStores.columns.name'),
        meta: { label: t('blobStores.columns.name'), hideable: false },
        cell: ({ row }) => (
          <Link
            to="/admin/blob-stores/$name"
            params={{ name: row.original.name }}
            className="font-mono text-link underline-offset-2 hover:underline"
          >
            {row.original.name}
          </Link>
        ),
      },
      {
        accessorKey: 'kind',
        header: t('blobStores.columns.kind'),
        meta: { label: t('blobStores.columns.kind') },
        cell: ({ row }) => <Badge>{t(`blobStores.kind.${row.original.kind}`)}</Badge>,
      },
      {
        id: 'usage',
        header: t('blobStores.columns.usage'),
        meta: { label: t('blobStores.columns.usage'), className: 'min-w-60' },
        cell: ({ row }) => (
          <div className="flex flex-col gap-1 py-1">
            <UsageBar view={row.original} />
            <QuotaBadge quota={usageDisplay(row.original).quota} quiet />
          </div>
        ),
      },
      {
        id: 'blobs',
        header: t('blobStores.columns.blobs'),
        meta: { label: t('blobStores.columns.blobs'), align: 'right' },
        cell: ({ row }) => row.original.usage?.blob_count.toLocaleString() ?? '-',
      },
      {
        id: 'free',
        header: t('blobStores.columns.free'),
        meta: { label: t('blobStores.columns.free'), align: 'right' },
        cell: ({ row }) => <BytesText bytes={row.original.usage?.available_bytes} />,
      },
      {
        id: 'health',
        header: t('blobStores.columns.health'),
        meta: { label: t('blobStores.columns.health') },
        cell: ({ row }) => (
          <div className="flex flex-col items-start gap-1 py-1">
            <HealthBadge health={row.original.health} />
            {row.original.health.last_error_code ? (
              <span className="font-mono text-xs text-muted-foreground">
                {row.original.health.last_error_code}
              </span>
            ) : null}
          </div>
        ),
      },
      {
        id: 'repositories',
        header: t('blobStores.columns.repositories'),
        meta: { label: t('blobStores.columns.repositories'), align: 'right' },
        cell: ({ row }) => row.original.repository_count,
      },
    ],
    [],
  )
}

function BlobStoreList() {
  const url = useUrlTableState(FILTER_IDS)
  const patchSearch = useSearchPatch()
  const { pageSize, setPageSize } = useTablePrefs('blob-stores')
  const { q, kind } = url.filters
  const columns = useColumns()

  const list = useInfinitePage<BlobStoreView>({
    // Filters and page size are part of the key: the `after` token only fits the same query.
    queryKey: [...blobStoreKeys.list({ q, kind }), pageSize],
    fetchPage: ({ after, signal }) => fetchBlobStorePage({ q, kind }, after, pageSize, signal),
  })

  const openCreate = () => patchSearch({ view: 'create' })

  return (
    <>
      <PageHeader
        title={t('blobStores.title')}
        description={t('blobStores.description')}
        actions={
          <PrivilegeGate need="admin">
            <Button onClick={openCreate}>
              <Plus aria-hidden="true" />
              {t('blobStores.create')}
            </Button>
          </PrivilegeGate>
        }
      />
      <DataTable<BlobStoreView>
        tableId="blob-stores"
        label={t('blobStores.tableLabel')}
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
            icon={HardDrive}
            title={t('blobStores.empty.title')}
            description={t('blobStores.empty.description')}
            action={
              <PrivilegeGate need="admin">
                <Button onClick={openCreate}>
                  <Plus aria-hidden="true" />
                  {t('blobStores.create')}
                </Button>
              </PrivilegeGate>
            }
          />
        }
      />
      <StorageTrendWidget compact className="mt-4" />
    </>
  )
}

/** `/admin/blob-stores`: the list, or the create view while `?view=create` is set. */
export function BlobStoresPage() {
  const view = useSearchParam('view')
  return view === 'create' ? <BlobStoreCreateView /> : <BlobStoreList />
}
