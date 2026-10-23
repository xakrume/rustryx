import { Link } from '@tanstack/react-router'
import { MoreHorizontal } from 'lucide-react'
import { useMemo, useState } from 'react'
import { api } from '@/api/client'
import { useInfinitePage } from '@/api/pagination'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'
import { BytesText } from '@/components/bytes-text'
import { ConfirmDialog } from '@/components/confirm-dialog'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { toast } from '@/lib/toast-store'
import { DemoSection } from '../demo-section'

type Repository = components['schemas']['RepositoryView']
type RepoFormat = components['schemas']['RepoFormat']
type RepoType = components['schemas']['RepoType']

const FILTER_IDS = ['q', 'format', 'repo_type'] as const

const FILTERS: readonly FilterDef[] = [
  { id: 'q', type: 'search', label: 'Name contains', placeholder: 'maven, team-0' },
  {
    id: 'format',
    type: 'select',
    label: 'Format',
    options: ['maven2', 'npm', 'pypi', 'docker', 'helm', 'raw', 'pub'].map((f) => ({
      value: f,
      label: f,
    })),
  },
  {
    id: 'repo_type',
    type: 'select',
    label: 'Type',
    options: ['hosted', 'proxy', 'group'].map((v) => ({ value: v, label: v })),
  },
]

/**
 * The reference list page: URL synced filters and sort, keyset pagination through the typed
 * client, the shared DataTable. A real list page copies this wiring and swaps the endpoint.
 */
export function TableSection() {
  const url = useUrlTableState(FILTER_IDS)
  const { pageSize, setPageSize } = useTablePrefs('demo-repositories')
  const [toDelete, setToDelete] = useState<Repository | null>(null)

  const { q, format, repo_type: repoType } = url.filters
  const sort = url.sort ?? 'name'

  const list = useInfinitePage<Repository>({
    // Every filter, the sort and the page size are in the key: the `after` token is only
    // valid for the same query, so any change must start from the first page.
    queryKey: ['demo', 'repositories', { q, format, repoType, sort, pageSize }],
    fetchPage: ({ after, signal }) =>
      unwrap(
        api.GET('/api/v1/repositories', {
          params: {
            query: {
              q,
              format: format as RepoFormat | undefined,
              repo_type: repoType as RepoType | undefined,
              sort: sort as 'name' | '-name' | 'created_at' | '-created_at',
              limit: pageSize,
              after: after ?? undefined,
              include: ['stats'],
            },
          },
          signal,
        }),
      ),
  })

  const columns = useMemo<AnyColumnDef<Repository>[]>(
    () => [
      {
        accessorKey: 'name',
        header: 'Name',
        meta: { label: 'Name', sortKey: 'name', hideable: false },
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
        accessorKey: 'format',
        header: 'Format',
        meta: { label: 'Format' },
        cell: ({ row }) => <span className="font-mono">{row.original.format}</span>,
      },
      { accessorKey: 'repo_type', header: 'Type', meta: { label: 'Type' } },
      {
        id: 'online',
        header: 'Status',
        meta: { label: 'Status' },
        cell: ({ row }) => (
          <StatusBadge
            status={row.original.online ? 'ok' : 'error'}
            label={row.original.online ? 'Online' : 'Offline'}
          />
        ),
      },
      {
        id: 'size',
        header: 'Size',
        meta: { label: 'Size', align: 'right' },
        cell: ({ row }) => <BytesText bytes={row.original.stats?.size_bytes} />,
      },
      {
        id: 'components',
        header: 'Components',
        meta: { label: 'Components', align: 'right' },
        cell: ({ row }) => row.original.stats?.component_count ?? '-',
      },
      {
        accessorKey: 'created_at',
        header: 'Created',
        meta: { label: 'Created', sortKey: 'created_at' },
        cell: ({ row }) => <RelativeTime value={row.original.created_at} />,
      },
      {
        id: 'actions',
        header: () => <span className="sr-only">Actions</span>,
        enableHiding: false,
        meta: { hideable: false, className: 'w-10' },
        cell: ({ row }) => (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={`Actions for ${row.original.name}`}>
                <MoreHorizontal aria-hidden="true" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem
                onSelect={() => void navigator.clipboard?.writeText(row.original.url)}
              >
                Copy URL
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setToDelete(row.original)}>Delete</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        ),
      },
    ],
    [],
  )

  return (
    <DemoSection
      id="table"
      title="Data table"
      description="Server side sort, filters and keyset paging against the mock API. State lives in the address bar."
    >
      <DataTable<Repository>
        tableId="demo-repositories"
        label="Repositories"
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
        selectable
        selectionLabel={(r) => `Select ${r.name}`}
        resetSelectionKey={JSON.stringify([q, format, repoType, sort])}
        toolbar={
          <FilterBar
            filters={FILTERS}
            values={url.filters}
            onChange={url.setFilter}
            onClear={url.clearFilters}
          />
        }
        bulkActions={({ selectedRows, clearSelection }) => (
          <Button
            variant="destructive"
            size="sm"
            onClick={() => {
              toast.info(`Would delete ${selectedRows.length} repositories`)
              clearSelection()
            }}
          >
            Delete selected
          </Button>
        )}
      />
      <ConfirmDialog
        open={toDelete !== null}
        onOpenChange={(open) => !open && setToDelete(null)}
        kind="repository"
        name={toDelete?.name ?? ''}
        requireTyping
        confirmLabel="Delete repository"
        onConfirm={() => {
          toast.success('Deleted (demo only)', { description: toDelete?.name })
        }}
      />
    </DemoSection>
  )
}
