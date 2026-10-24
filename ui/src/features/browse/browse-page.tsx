import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate, useParams, useSearch } from '@tanstack/react-router'
import { FolderTree, PackageOpen } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { isApiError } from '@/api/errors'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { FilterBar, type FilterDef } from '@/components/data-table'
import { Tree } from '@/components/tree/tree'
import type { TreeNodeData } from '@/components/tree/tree-model'
import { Skeleton } from '@/components/ui/skeleton'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { repositoryQuery } from '@/features/repositories/api'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { useMediaQuery } from '@/hooks/use-media-query'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { browseKeys, fetchFolder, folderQuery } from './api'
import { DetailPane } from './detail-pane'
import { NodeLabel } from './node-label'
import { cleanPath, dataOf, nodesOf, revealIdsFor, ROOT_ID, rootNode } from './tree-nodes'

const CRUMB =
  'cursor-pointer rounded-sm text-link underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring'

/** The selected path as links, so a folder above the file is one click (and one tab stop) away. */
function PathTrail({
  repository,
  path,
  onSelect,
}: {
  repository: string
  path: string | undefined
  onSelect: (next: string | undefined) => void
}) {
  const parts = path === undefined ? [] : path.split('/').filter(Boolean)
  const folder = path?.endsWith('/') === true
  return (
    <nav aria-label={t('browse.pathLabel')}>
      <ol className="m-0 flex list-none flex-wrap items-center gap-x-1 gap-y-1 p-0 font-mono text-sm">
        <li className="flex items-center gap-1">
          {parts.length === 0 ? (
            <span aria-current="page">{repository}</span>
          ) : (
            <button type="button" className={CRUMB} onClick={() => onSelect(undefined)}>
              {repository}
            </button>
          )}
        </li>
        {parts.map((part, index) => {
          const last = index === parts.length - 1
          const target = `/${parts.slice(0, index + 1).join('/')}${last && !folder ? '' : '/'}`
          return (
            <li key={target} className="flex min-w-0 items-center gap-1">
              <span aria-hidden="true" className="text-muted-foreground">
                /
              </span>
              {last ? (
                <span className="break-all" aria-current="page">
                  {part}
                </span>
              ) : (
                <button
                  type="button"
                  className={`${CRUMB} break-all`}
                  onClick={() => onSelect(target)}
                >
                  {part}
                </button>
              )}
            </li>
          )
        })}
      </ol>
    </nav>
  )
}

function UnsupportedFormat({ repository }: { repository: string }) {
  return (
    <EmptyState
      icon={PackageOpen}
      title={t('browse.noTreeTitle')}
      description={t('browse.noTreeBody')}
      action={
        <Link
          to="/repositories/$name/components"
          params={{ name: repository }}
          search={{}}
          className="text-link underline underline-offset-2"
        >
          {t('browse.openComponents')}
        </Link>
      }
    />
  )
}

/**
 * `/repositories/:name/browse`: a lazy ARIA tree beside a detail pane. A folder is fetched when
 * it is first opened, one level and one page at a time; a folder with more entries ends in a
 * "load more" row. The selection lives in the address bar (`path`), so it survives a reload and
 * can be shared: the tree opens the folders on the way to it. The name filter (`q`) is a prefix
 * filter applied to every folder.
 */
export function BrowsePage() {
  const { name } = useParams({ from: '/_app/repositories/$name/browse' })
  const search = useSearch({ from: '/_app/repositories/$name/browse' })
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const repo = useQuery(repositoryQuery(name)).data?.data
  const wide = useMediaQuery('(min-width: 1024px)')
  const paneRef = useRef<HTMLElement | null>(null)
  const [version, setVersion] = useState(0)
  const [hint, setHint] = useState<{ path: string; assetId: string } | null>(null)

  const path = cleanPath(search.path)
  const q = search.q
  // The first page of the root tells the page whether there is anything to show before the tree
  // renders; the tree reuses this cached answer when it opens the root.
  const root = useQuery(folderQuery(name, ROOT_ID, q))
  const showSkeleton = useDelayedFlag(root.isPending, 150)

  const setSearch = useCallback(
    (changes: { path?: string | undefined; q?: string | undefined }) => {
      void navigate({
        to: '.',
        replace: true,
        search: ((previous: Record<string, unknown>) => ({ ...previous, ...changes })) as never,
      })
    },
    [navigate],
  )

  const refresh = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: browseKeys.all })
    setVersion((v) => v + 1)
  }, [queryClient])

  const loadFolder = useCallback(
    async (node: TreeNodeData): Promise<TreeNodeData[]> => {
      const page = await queryClient.fetchQuery(folderQuery(name, node.id, q))
      return nodesOf(page, node.id)
    },
    [name, q, queryClient],
  )
  const loadMore = useCallback(
    async (node: TreeNodeData, signal: AbortSignal): Promise<TreeNodeData[]> => {
      const data = dataOf(node)
      if (data?.kind !== 'more') return []
      const page = await fetchFolder(name, data.folder, q, data.after, signal)
      return nodesOf(page, data.folder, data.loaded)
    },
    [name, q],
  )

  const roots = useMemo(() => [rootNode(name)], [name])
  const reveal = useMemo(() => revealIdsFor(path), [path])
  const assetId = hint !== null && hint.path === path ? hint.assetId : null

  const select = useCallback(
    (node: TreeNodeData) => {
      const data = dataOf(node)
      if (data?.kind === 'asset' && data.entry.asset_id) {
        setHint({ path: node.id, assetId: data.entry.asset_id })
      }
      setSearch({ path: node.id })
    },
    [setSearch],
  )

  // On a narrow screen the pane sits below the tree: bring it into view when something is chosen.
  useEffect(() => {
    if (path !== undefined && !wide) paneRef.current?.scrollIntoView({ block: 'start' })
  }, [path, wide])

  const onGone = useCallback(() => {
    toast.warning(t('browse.itemGone'))
    setSearch({ path: undefined })
    refresh()
  }, [refresh, setSearch])

  const filters = useMemo<FilterDef[]>(
    () => [
      {
        id: 'q',
        type: 'search',
        label: t('browse.filterLabel'),
        placeholder: t('browse.filterPlaceholder'),
      },
    ],
    [],
  )

  if (root.error) {
    if (isApiError(root.error) && root.error.code === 'unsupported_for_format') {
      return <UnsupportedFormat repository={name} />
    }
    return (
      <ErrorPanel
        error={root.error}
        title={t('browse.loadFailed')}
        onRetry={() => void root.refetch()}
      />
    )
  }

  if (root.data !== undefined && root.data.items.length === 0 && q === undefined) {
    return (
      <div className="space-y-4">
        <EmptyState
          icon={FolderTree}
          title={t('browse.emptyTitle')}
          description={
            repo?.repo_type === 'hosted' ? t('browse.emptyHosted') : t('browse.emptyOther')
          }
        />
        {repo?.repo_type === 'hosted' ? <ClientSetupPanel repository={name} /> : null}
      </div>
    )
  }

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(18rem,26rem)_minmax(0,1fr)]">
      <h2 className="sr-only">{t('pages.browse')}</h2>
      <div className="min-w-0 space-y-3">
        <FilterBar
          filters={filters}
          values={{ q }}
          onChange={(_, value) => setSearch({ q: value ?? undefined })}
          onClear={() => setSearch({ q: undefined })}
        />
        <PathTrail repository={name} path={path} onSelect={(next) => setSearch({ path: next })} />
        <div
          className="max-h-[60dvh] overflow-auto rounded-lg border border-border bg-card p-1 lg:max-h-[calc(100dvh-16rem)]"
          aria-busy={root.isPending}
        >
          {root.isPending ? (
            showSkeleton ? (
              <div className="space-y-2 p-2" aria-hidden="true">
                {Array.from({ length: 6 }, (_, i) => (
                  <Skeleton key={i} className="h-6" />
                ))}
              </div>
            ) : null
          ) : (
            <Tree
              key={`${name}|${q ?? ''}|${version}`}
              label={t('browse.treeLabel', { name })}
              roots={roots}
              loadChildren={loadFolder}
              loadMore={loadMore}
              revealIds={reveal}
              selectedId={path ?? null}
              onSelect={select}
              renderLabel={(node, row) => <NodeLabel node={node} expanded={row.expanded} />}
            />
          )}
        </div>
        {q !== undefined && root.data?.items.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('browse.noMatch', { q })}</p>
        ) : null}
      </div>
      <section
        ref={paneRef}
        aria-label={t('browse.detailsLabel')}
        className="min-w-0 rounded-lg border border-border bg-card p-4"
      >
        <DetailPane
          repository={name}
          path={path}
          assetId={assetId}
          onDeleted={() => {
            setSearch({ path: undefined })
            refresh()
          }}
          onGone={onGone}
        />
      </section>
    </div>
  )
}
