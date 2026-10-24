import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from '@tanstack/react-router'
import { Package } from 'lucide-react'
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
import { RelativeTime } from '@/components/relative-time'
import { repositoryQuery } from '@/features/repositories/api'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { componentListConfig, componentName, type ComponentView } from './api'

const FILTER_IDS = ['q', 'group', 'version'] as const

function isImage(format: string): boolean {
  return format === 'docker' || format === 'oci'
}

function buildColumns(repository: string, images: boolean): AnyColumnDef<ComponentView>[] {
  const name: AnyColumnDef<ComponentView> = {
    id: 'name',
    header: images ? t('components.image') : t('components.name'),
    meta: {
      label: images ? t('components.image') : t('components.name'),
      sortKey: 'name',
      hideable: false,
    },
    cell: ({ row }) =>
      images ? (
        <Link
          to="/repositories/$name/images/$image"
          params={{ name: repository, image: row.original.name }}
          search={{}}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {row.original.name}
        </Link>
      ) : (
        <Link
          to="/components/$id"
          params={{ id: row.original.id }}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {componentName(row.original)}
        </Link>
      ),
  }
  const version: AnyColumnDef<ComponentView> = {
    id: 'version',
    header: images ? t('components.tag') : t('components.version'),
    meta: { label: images ? t('components.tag') : t('components.version') },
    cell: ({ row }) => <span className="font-mono">{row.original.version ?? '-'}</span>,
  }
  const tagCount: AnyColumnDef<ComponentView> = {
    id: 'tags',
    header: t('components.tagCount'),
    meta: { label: t('components.tagCount'), align: 'right' },
    cell: ({ row }) => {
      const value = row.original.attributes_summary.tag_count
      return typeof value === 'number' ? value.toLocaleString() : '-'
    },
  }
  const tail: AnyColumnDef<ComponentView>[] = [
    {
      id: 'assets',
      header: t('components.assetCount'),
      meta: { label: t('components.assetCount'), align: 'right' },
      cell: ({ row }) => row.original.asset_count.toLocaleString(),
    },
    {
      id: 'size',
      header: t('components.size'),
      meta: { label: t('components.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.size_bytes} />,
    },
    {
      id: 'updated',
      header: t('components.updated'),
      meta: { label: t('components.updated'), sortKey: 'updated_at' },
      cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
    },
  ]
  const namespace: AnyColumnDef<ComponentView> = {
    id: 'group',
    header: t('components.namespace'),
    meta: { label: t('components.namespace') },
    cell: ({ row }) => <span className="font-mono">{row.original.group_id ?? '-'}</span>,
  }
  return images ? [name, version, tagCount, ...tail] : [name, namespace, version, ...tail]
}

/**
 * The flat list of every component of a repository, newest first, with name, namespace and
 * version filters in the address bar. Docker and OCI repositories show the image, its tag and
 * the tag count from `attributes_summary`, and the name opens the image page.
 */
export function RepositoryComponentsPage() {
  const { name } = useParams({ from: '/_app/repositories/$name/components' })
  const repo = useQuery(repositoryQuery(name)).data?.data
  const url = useUrlTableState(FILTER_IDS)
  const { pageSize, setPageSize } = useTablePrefs('repository-components')
  const sort = url.sort ?? '-created_at'
  const list = useInfinitePage<ComponentView>(
    componentListConfig(name, { ...url.filters, sort }, pageSize),
  )
  const images = repo !== undefined && isImage(repo.format)
  const columns = useMemo(() => buildColumns(name, images), [name, images])
  const filters = useMemo<FilterDef[]>(
    () => [
      {
        id: 'q',
        type: 'search',
        label: t('components.filterSearch'),
        placeholder: t('components.filterSearchPlaceholder'),
      },
      ...(images
        ? []
        : [{ id: 'group', type: 'search', label: t('components.namespace') } satisfies FilterDef]),
      {
        id: 'version',
        type: 'search',
        label: images ? t('components.tag') : t('components.version'),
      },
    ],
    [images],
  )
  return (
    <div className="space-y-3">
      <h2 className="sr-only">{t('pages.components')}</h2>
      <DataTable<ComponentView>
        tableId={`components-${images ? 'images' : 'packages'}`}
        label={t('pages.components')}
        columns={columns}
        rows={list.items}
        getRowId={(c) => c.id}
        isLoading={list.isLoading}
        error={list.isError ? list.error : undefined}
        onRetry={list.refetch}
        hasNextPage={list.hasNextPage}
        isLoadingMore={list.isLoadingMore}
        onLoadMore={list.loadMore}
        sort={sort}
        onSortChange={url.setSort}
        hasActiveFilters={url.hasActiveFilters}
        onClearFilters={url.clearFilters}
        pageSize={pageSize}
        onPageSizeChange={setPageSize}
        emptyState={
          <EmptyState
            icon={Package}
            title={t('components.emptyTitle')}
            description={
              repo?.repo_type === 'hosted'
                ? t('components.emptyHosted')
                : t('components.emptyOther')
            }
            action={
              repo?.permissions.write && repo.repo_type === 'hosted' ? (
                <Link
                  to="/upload"
                  search={{ repository: name }}
                  className="text-link underline underline-offset-2"
                >
                  {t('repos.upload')}
                </Link>
              ) : undefined
            }
          />
        }
        toolbar={
          <FilterBar
            filters={filters}
            values={url.filters}
            onChange={url.setFilter}
            onClear={url.clearFilters}
          />
        }
      />
    </div>
  )
}
