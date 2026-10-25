import { Link } from '@tanstack/react-router'
import { Boxes, MoreHorizontal, Plus } from 'lucide-react'
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
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { usePermissions } from '@/features/auth/use-permissions'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { repositoryListConfig, type RepositoryView } from './api'
import { DeleteRepositoryDialog } from './delete-repository-dialog'
import { FormatBadge, OnlineBadge, RepoTypeBadge, UpstreamBadge, formatLabel } from './repo-meta'

const FILTER_IDS = ['q', 'format', 'repo_type', 'online'] as const
const FORMATS = ['maven2', 'npm', 'pypi', 'docker', 'oci', 'helm', 'raw', 'pub'] as const
const TYPES = ['hosted', 'proxy', 'group'] as const

function buildFilters(): FilterDef[] {
  return [
    {
      id: 'q',
      type: 'search',
      label: t('repos.filters.name'),
      placeholder: t('repos.filters.namePlaceholder'),
    },
    {
      id: 'format',
      type: 'select',
      label: t('repos.filters.format'),
      options: FORMATS.map((value) => ({ value, label: formatLabel(value) })),
    },
    {
      id: 'repo_type',
      type: 'select',
      label: t('repos.filters.type'),
      options: TYPES.map((value) => ({ value, label: t(`repos.type.${value}`) })),
    },
    {
      id: 'online',
      type: 'select',
      label: t('repos.filters.status'),
      options: [
        { value: 'true', label: t('status.online') },
        { value: 'false', label: t('status.offline') },
      ],
    },
  ]
}

interface RowActions {
  onClientSetup: (name: string) => void
  onDelete: (name: string) => void
}

function RowMenu({ repo, actions }: { repo: RepositoryView; actions: RowActions }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('repos.list.actionsFor', { name: repo.name })}
        >
          <MoreHorizontal aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuItem asChild>
          <Link to="/repositories/$name" params={{ name: repo.name }}>
            {t('repos.list.open')}
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => actions.onClientSetup(repo.name)}>
          {t('repos.list.clientSetup')}
        </DropdownMenuItem>
        {repo.permissions.admin ? (
          <>
            <DropdownMenuItem asChild>
              <Link to="/repositories/$name/settings" params={{ name: repo.name }}>
                {t('common.edit')}
              </Link>
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => actions.onDelete(repo.name)}>
              <span className="text-destructive-text">{t('common.delete')}</span>
            </DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function buildColumns(actions: RowActions): AnyColumnDef<RepositoryView>[] {
  return [
    {
      accessorKey: 'name',
      header: t('repos.columns.name'),
      meta: { label: t('repos.columns.name'), sortKey: 'name', hideable: false },
      cell: ({ row }) => (
        <Link
          to="/repositories/$name"
          params={{ name: row.original.name }}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {row.original.name}
        </Link>
      ),
    },
    {
      id: 'format',
      header: t('repos.columns.format'),
      meta: { label: t('repos.columns.format') },
      cell: ({ row }) => <FormatBadge format={row.original.format} />,
    },
    {
      id: 'type',
      header: t('repos.columns.type'),
      meta: { label: t('repos.columns.type') },
      cell: ({ row }) => <RepoTypeBadge type={row.original.repo_type} />,
    },
    {
      id: 'status',
      header: t('repos.columns.status'),
      meta: { label: t('repos.columns.status') },
      cell: ({ row }) => (
        <span className="flex items-center gap-1.5">
          <OnlineBadge online={row.original.online} />
          {row.original.upstream && row.original.upstream.state !== 'ok' ? (
            <UpstreamBadge state={row.original.upstream.state} />
          ) : null}
        </span>
      ),
    },
    {
      id: 'blob_store',
      header: t('repos.columns.blobStore'),
      meta: { label: t('repos.columns.blobStore') },
      cell: ({ row }) => <span className="font-mono">{row.original.blob_store.name}</span>,
    },
    {
      id: 'size',
      header: t('repos.columns.size'),
      meta: { label: t('repos.columns.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.stats?.size_bytes} />,
    },
    {
      id: 'components',
      header: t('repos.columns.components'),
      meta: { label: t('repos.columns.components'), align: 'right' },
      cell: ({ row }) =>
        row.original.stats ? row.original.stats.component_count.toLocaleString() : '-',
    },
    {
      id: 'health',
      header: t('repos.columns.health'),
      meta: { label: t('repos.columns.health') },
      cell: ({ row }) => {
        const health = row.original.health
        if (!health?.scanned) {
          return <span className="text-muted-foreground">{t('repos.health.notScanned')}</span>
        }
        const checked =
          health.component_count === 0
            ? '0'
            : `${health.scanned_components}/${health.component_count}`
        return (
          <span title={health.scanned_at ?? undefined}>
            {health.critical} / {health.severe} / {health.moderate}
            <span className="ml-2 text-muted-foreground">{checked}</span>
          </span>
        )
      },
    },
    {
      id: 'url',
      header: t('repos.columns.url'),
      meta: { label: t('repos.columns.url') },
      cell: ({ row }) => (
        <span className="flex items-center gap-1">
          <span
            className="max-w-64 truncate font-mono text-muted-foreground"
            title={row.original.url}
          >
            {row.original.url}
          </span>
          <CopyButton
            value={row.original.url}
            what={t('repos.copyUrlOf', { name: row.original.name })}
          />
        </span>
      ),
    },
    {
      accessorKey: 'created_at',
      header: t('repos.columns.created'),
      meta: { label: t('repos.columns.created'), sortKey: 'created_at' },
      cell: ({ row }) => <RelativeTime value={row.original.created_at} />,
    },
    {
      id: 'actions',
      header: () => <span className="sr-only">{t('repos.columns.actions')}</span>,
      enableHiding: false,
      meta: { hideable: false, className: 'w-10' },
      cell: ({ row }) => <RowMenu repo={row.original} actions={actions} />,
    },
  ]
}

/**
 * The repository list: name, format, type, status, blob store, usage (size and component count
 * from `include=stats`) and URL with a copy button. Filters, sort and the page size are in the
 * address bar and server side; paging is by keyset ("load more"). Row actions follow the
 * repository's own `permissions`, so editing and deleting are offered to administrators only.
 */
export function RepositoriesPage() {
  const url = useUrlTableState(FILTER_IDS)
  const { pageSize, setPageSize } = useTablePrefs('repositories')
  const permissions = usePermissions()
  const [setupFor, setSetupFor] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<string | null>(null)

  const sort = url.sort ?? 'name'
  const list = useInfinitePage<RepositoryView>(
    repositoryListConfig({ ...url.filters, sort }, pageSize),
  )
  const columns = useMemo(
    () => buildColumns({ onClientSetup: setSetupFor, onDelete: setDeleting }),
    [],
  )
  const filters = useMemo(buildFilters, [])

  const create = (
    <Button asChild>
      <Link to="/repositories/new">
        <Plus aria-hidden="true" />
        {t('repos.create')}
      </Link>
    </Button>
  )

  return (
    <>
      <PageHeader
        title={t('nav.items.repositories')}
        description={t('repos.list.description')}
        actions={permissions.can.create_repository ? create : undefined}
      />
      <DataTable<RepositoryView>
        tableId="repositories"
        label={t('nav.items.repositories')}
        columns={columns}
        rows={list.items}
        getRowId={(r) => r.id}
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
            icon={Boxes}
            title={
              permissions.can.create_repository
                ? t('repos.list.emptyAdminTitle')
                : t('repos.list.emptyUserTitle')
            }
            description={
              permissions.can.create_repository
                ? t('repos.list.emptyAdminBody')
                : t('repos.list.emptyUserBody')
            }
            action={permissions.can.create_repository ? create : undefined}
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

      <Sheet open={setupFor !== null} onOpenChange={(open) => !open && setSetupFor(null)}>
        <SheetContent side="right" className="sm:w-[40rem]">
          <SheetHeader className="border-b border-border p-4 pr-14">
            <SheetTitle>{t('repos.list.clientSetupTitle', { name: setupFor ?? '' })}</SheetTitle>
            <SheetDescription>{t('repos.list.clientSetupBody')}</SheetDescription>
          </SheetHeader>
          <div className="min-h-0 flex-1 overflow-y-auto p-4">
            {setupFor !== null ? <ClientSetupPanel repository={setupFor} hideTitle /> : null}
          </div>
        </SheetContent>
      </Sheet>
      <DeleteRepositoryDialog name={deleting} onClose={() => setDeleting(null)} />
    </>
  )
}
