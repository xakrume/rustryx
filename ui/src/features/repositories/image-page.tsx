import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from '@tanstack/react-router'
import { Tag, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { RelativeTime } from '@/components/relative-time'
import { Button } from '@/components/ui/button'
import { componentListConfig, type ComponentView } from '@/features/components/api'
import { DeleteComponentDialog } from '@/features/components/delete-dialogs'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { repositoryQuery } from './api'

/** `docker pull host/repository/image:tag`: the registry host is the host of the repository URL. */
function pullCommand(
  repositoryUrl: string,
  repository: string,
  image: string,
  tag: string,
): string {
  return `docker pull ${new URL(repositoryUrl).host}/${repository}/${image}:${tag}`
}

interface RowActions {
  repository: string
  url: string
  canDelete: boolean
  onDelete: (component: ComponentView) => void
}

function buildColumns({
  repository,
  url,
  canDelete,
  onDelete,
}: RowActions): AnyColumnDef<ComponentView>[] {
  return [
    {
      id: 'tag',
      header: t('components.tag'),
      meta: { label: t('components.tag'), hideable: false },
      cell: ({ row }) => (
        <Link
          to="/components/$id"
          params={{ id: row.original.id }}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {row.original.version ?? '-'}
        </Link>
      ),
    },
    {
      id: 'digest',
      header: t('images.digest'),
      meta: { label: t('images.digest') },
      cell: ({ row }) => {
        const digest = row.original.attributes_summary.digest
        return typeof digest === 'string' && digest !== '' ? (
          <span className="flex items-center gap-1">
            <code className="font-mono text-muted-foreground" title={digest}>
              {digest.slice(0, 19)}
            </code>
            <CopyButton
              value={digest}
              what={t('images.digestOf', { tag: row.original.version ?? '' })}
            />
          </span>
        ) : (
          '-'
        )
      },
    },
    {
      id: 'size',
      header: t('components.size'),
      meta: { label: t('components.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.size_bytes} />,
    },
    {
      id: 'pushed',
      header: t('images.pushed'),
      meta: { label: t('images.pushed'), sortKey: 'created_at' },
      cell: ({ row }) => <RelativeTime value={row.original.created_at} />,
    },
    {
      id: 'actions',
      header: () => <span className="sr-only">{t('repos.columns.actions')}</span>,
      enableHiding: false,
      meta: { hideable: false },
      cell: ({ row }) => {
        const tag = row.original.version ?? 'latest'
        return (
          <span className="flex items-center gap-1">
            <CopyButton
              value={pullCommand(url, repository, row.original.name, tag)}
              what={t('images.pullOf', { name: row.original.name, tag })}
              showLabel
              variant="outline"
            />
            {canDelete ? (
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('images.deleteTag', { name: row.original.name, tag })}
                onClick={() => onDelete(row.original)}
              >
                <Trash2 aria-hidden="true" />
              </Button>
            ) : null}
          </span>
        )
      },
    },
  ]
}

/**
 * Basic image page (docs/ui-page-inventory.md, 7.2): the tags of one image with digest, size and
 * push time, a tag filter and sort in the address bar, a pull command to copy for each, delete
 * for those who may, and the client setup panel. Signatures, the multi-arch children, the
 * untagged filter and retagging are the later, full version of this page.
 */
export function ImagePage() {
  const { name, image } = useParams({ from: '/_app/repositories/$name/images/$image' })
  const repo = useQuery(repositoryQuery(name)).data?.data
  const url = useUrlTableState(['q'])
  const { pageSize, setPageSize } = useTablePrefs('image-tags')
  const [deleting, setDeleting] = useState<ComponentView | null>(null)
  const sort = url.sort ?? '-created_at'
  const list = useInfinitePage<ComponentView>(
    componentListConfig(name, { name: image, q: url.filters.q, sort }, pageSize),
  )
  const filters = useMemo<FilterDef[]>(
    () => [
      {
        id: 'q',
        type: 'search',
        label: t('images.filterTag'),
        placeholder: t('images.filterTagPlaceholder'),
      },
    ],
    [],
  )
  const columns = useMemo(
    () =>
      buildColumns({
        repository: name,
        url: repo?.url ?? `https://localhost/repository/${name}/`,
        canDelete: repo?.permissions.delete === true,
        onDelete: setDeleting,
      }),
    [name, repo],
  )
  return (
    <div className="space-y-3">
      <div>
        <h2 className="font-mono text-lg font-semibold break-all">{image}</h2>
        <p className="text-sm text-muted-foreground">{t('images.description')}</p>
      </div>
      <DataTable<ComponentView>
        tableId="image-tags"
        label={t('images.tags', { image })}
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
        toolbar={
          <FilterBar
            filters={filters}
            values={url.filters}
            onChange={url.setFilter}
            onClear={url.clearFilters}
          />
        }
        emptyState={
          <EmptyState
            icon={Tag}
            title={t('images.emptyTitle')}
            description={t('images.emptyBody')}
            action={
              <Link
                to="/repositories/$name/components"
                params={{ name }}
                search={{}}
                className="text-link underline underline-offset-2"
              >
                {t('browse.openComponents')}
              </Link>
            }
          />
        }
      />
      <ClientSetupPanel repository={name} />
      <DeleteComponentDialog
        component={
          deleting
            ? {
                id: deleting.id,
                label: `${deleting.name}:${deleting.version ?? ''}`,
                assets: deleting.asset_count,
              }
            : null
        }
        onClose={() => setDeleting(null)}
      />
    </div>
  )
}
