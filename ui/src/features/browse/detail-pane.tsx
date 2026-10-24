import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { FileSearch, Folder } from 'lucide-react'
import { useEffect } from 'react'
import { isApiError } from '@/api/errors'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { Skeleton } from '@/components/ui/skeleton'
import { AssetDetailsView, SectionTitle } from '@/features/components/asset-details'
import { assetQuery, componentLabel, componentQuery } from '@/features/components/api'
import { Fact, FactList } from '@/features/components/fact-list'
import { t } from '@/i18n'
import { assetByPathQuery } from './api'

function ComponentSummary({ id }: { id: string }) {
  const query = useQuery(componentQuery(id))
  const component = query.data
  if (component === undefined) {
    return query.error ? null : <Skeleton className="h-16" aria-hidden="true" />
  }
  return (
    <section className="rounded-md border border-border bg-muted p-3">
      <SectionTitle level={3}>{t('browse.component')}</SectionTitle>
      <FactList>
        <Fact label={t('components.name')} mono>
          <Link
            to="/components/$id"
            params={{ id: component.id }}
            className="text-link underline-offset-2 hover:underline"
          >
            {componentLabel(component)}
          </Link>
        </Fact>
        <Fact label={t('components.assetCount')}>{component.asset_count.toLocaleString()}</Fact>
        <Fact label={t('components.size')}>
          <BytesText bytes={component.size_bytes} />
        </Fact>
      </FactList>
    </section>
  )
}

export interface DetailPaneProps {
  repository: string
  /** Selected path: `/dir/` for a folder, `/dir/file` for a file, undefined for nothing. */
  path: string | undefined
  /** The asset id when the tree knows it (a click), so no lookup by path is needed. */
  assetId: string | null
  /** The file was deleted here. */
  onDeleted: () => void
  /** The file no longer exists (404): the page tells the user and refreshes the tree. */
  onGone: () => void
}

function AssetPane({
  repository,
  path,
  assetId,
  onDeleted,
  onGone,
}: DetailPaneProps & { path: string }) {
  const lookup = useQuery(assetByPathQuery(repository, path, assetId === null))
  const id = assetId ?? lookup.data?.id ?? null
  const query = useQuery({ ...assetQuery(id ?? ''), enabled: id !== null })
  const missing =
    (lookup.isSuccess && lookup.data === null) ||
    (isApiError(query.error) && query.error.status === 404)

  useEffect(() => {
    if (missing) onGone()
  }, [missing, onGone])

  if (query.data !== undefined) {
    return (
      <div className="space-y-4">
        {query.data.component_id ? <ComponentSummary id={query.data.component_id} /> : null}
        <AssetDetailsView asset={query.data} level={3} showOpenLink onDeleted={onDeleted} />
      </div>
    )
  }
  const error = query.error ?? lookup.error
  if (error && !missing) {
    return (
      <ErrorPanel
        error={error}
        title={t('browse.detailFailed')}
        onRetry={() => void (query.error ? query.refetch() : lookup.refetch())}
      />
    )
  }
  return missing ? null : (
    <div aria-busy="true" className="space-y-3">
      <Skeleton className="h-8 w-64" />
      <Skeleton className="h-40" />
    </div>
  )
}

function FolderPane({ repository, path }: { repository: string; path: string }) {
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <Folder className="size-5 text-muted-foreground" aria-hidden="true" />
        <p className="min-w-0 font-mono text-sm break-all">{path === '/' ? repository : path}</p>
        <CopyButton value={path} what={t('browse.folderPath')} />
      </div>
      <FactList>
        <Fact label={t('components.repository')} mono>
          {repository}
        </Fact>
        <Fact label={t('components.path')} mono>
          {path}
        </Fact>
      </FactList>
      <p className="text-sm text-muted-foreground">{t('browse.folderHint')}</p>
    </div>
  )
}

/** The pane next to the tree: a file with all its facts, a folder with its path, or a hint. */
export function DetailPane(props: DetailPaneProps) {
  const { repository, path } = props
  if (path === undefined) {
    return (
      <EmptyState
        icon={FileSearch}
        title={t('browse.nothingSelected')}
        description={t('browse.nothingSelectedBody')}
      />
    )
  }
  return path.endsWith('/') ? (
    <FolderPane repository={repository} path={path} />
  ) : (
    <AssetPane {...props} path={path} />
  )
}
