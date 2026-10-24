import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import { PanelRight, SearchX, TextSearch, X } from 'lucide-react'
import { useCallback, useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import { isApiError } from '@/api/errors'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import { DataTable, useTablePrefs, type AnyColumnDef } from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { componentName } from '@/features/components/api'
import { FormatBadge } from '@/features/repositories/repo-meta'
import { t } from '@/i18n'
import {
  assetSearchConfig,
  componentSearchConfig,
  hasCriteria,
  type AssetView,
  type ComponentView,
  type SearchCriteria,
} from './api'
import { CriteriaForm, criterionLabel } from './criteria-form'
import { DetailSheet, type Selection } from './detail-sheet'

const EXAMPLES: readonly { label: string; search: Partial<SearchCriteria> }[] = [
  { label: 'api*', search: { q: 'api*' } },
  {
    label: 'format:maven2 group:com.example.core',
    search: { format: 'maven2', group: 'com.example.core' },
  },
  { label: 'name:api-core', search: { name: 'api-core' } },
  { label: 'repository:npm-releases', search: { repository: 'npm-releases' } },
]

function RepositoryLink({ name }: { name: string }) {
  return (
    <Link
      to="/repositories/$name"
      params={{ name }}
      className="font-mono text-link underline-offset-2 hover:underline"
    >
      {name}
    </Link>
  )
}

function componentColumns(open: (selection: Selection) => void): AnyColumnDef<ComponentView>[] {
  return [
    {
      id: 'name',
      header: t('components.name'),
      meta: { label: t('components.name'), hideable: false },
      cell: ({ row }) => (
        <Link
          to="/components/$id"
          params={{ id: row.original.id }}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {componentName(row.original)}
        </Link>
      ),
    },
    {
      id: 'version',
      header: t('components.version'),
      meta: { label: t('components.version') },
      cell: ({ row }) => <span className="font-mono">{row.original.version ?? '-'}</span>,
    },
    {
      id: 'repository',
      header: t('components.repository'),
      meta: { label: t('components.repository') },
      cell: ({ row }) => <RepositoryLink name={row.original.repository} />,
    },
    {
      id: 'format',
      header: t('search.format'),
      meta: { label: t('search.format') },
      cell: ({ row }) => <FormatBadge format={row.original.format} />,
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
      meta: { label: t('components.updated') },
      cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
    },
    {
      id: 'details',
      header: () => <span className="sr-only">{t('search.details')}</span>,
      enableHiding: false,
      meta: { hideable: false, className: 'w-10' },
      cell: ({ row }) => (
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('search.detailsOf', { name: componentName(row.original) })}
          onClick={() =>
            open({ kind: 'component', id: row.original.id, title: componentName(row.original) })
          }
        >
          <PanelRight aria-hidden="true" />
        </Button>
      ),
    },
  ]
}

function assetColumns(open: (selection: Selection) => void): AnyColumnDef<AssetView>[] {
  return [
    {
      id: 'path',
      header: t('components.path'),
      meta: { label: t('components.path'), hideable: false },
      cell: ({ row }) => (
        <Link
          to="/assets/$id"
          params={{ id: row.original.id }}
          className="font-mono text-link underline-offset-2 hover:underline"
        >
          {row.original.path}
        </Link>
      ),
    },
    {
      id: 'repository',
      header: t('components.repository'),
      meta: { label: t('components.repository') },
      cell: ({ row }) => <RepositoryLink name={row.original.repository} />,
    },
    {
      id: 'type',
      header: t('components.contentType'),
      meta: { label: t('components.contentType') },
      cell: ({ row }) => (
        <span className="font-mono text-muted-foreground">{row.original.content_type}</span>
      ),
    },
    {
      id: 'size',
      header: t('components.size'),
      meta: { label: t('components.size'), align: 'right' },
      cell: ({ row }) => <BytesText bytes={row.original.size_bytes} />,
    },
    {
      id: 'sha256',
      header: 'SHA-256',
      meta: { label: 'SHA-256' },
      cell: ({ row }) => (
        <span className="flex items-center gap-1">
          <code className="font-mono text-muted-foreground" title={row.original.checksums.sha256}>
            {row.original.checksums.sha256.slice(0, 12)}
          </code>
          <CopyButton
            value={row.original.checksums.sha256}
            what={t('components.checksumOf', { algorithm: 'SHA-256' })}
          />
        </span>
      ),
    },
    {
      id: 'updated',
      header: t('components.updated'),
      meta: { label: t('components.updated') },
      cell: ({ row }) => <RelativeTime value={row.original.updated_at} />,
    },
    {
      id: 'details',
      header: () => <span className="sr-only">{t('search.details')}</span>,
      enableHiding: false,
      meta: { hideable: false, className: 'w-10' },
      cell: ({ row }) => (
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('search.detailsOf', { name: row.original.path })}
          onClick={() => open({ kind: 'asset', id: row.original.id, title: row.original.path })}
        >
          <PanelRight aria-hidden="true" />
        </Button>
      ),
    },
  ]
}

const CRITERIA_FIELDS = [
  'q',
  'format',
  'repository',
  'group',
  'name',
  'version',
  'checksum',
  'path',
] as const satisfies readonly (keyof SearchCriteria)[]

function ActiveCriteria({
  criteria,
  onRemove,
}: {
  criteria: SearchCriteria
  onRemove: (field: keyof SearchCriteria) => void
}) {
  const active = CRITERIA_FIELDS.filter((field) => criteria[field] !== undefined)
  if (active.length === 0) return null
  return (
    <ul aria-label={t('search.activeCriteria')} className="m-0 flex list-none flex-wrap gap-2 p-0">
      {active.map((field) => {
        const label = criterionLabel(field, criteria.format)
        return (
          <li
            key={field}
            className="inline-flex max-w-full items-center gap-1 rounded-md border border-border bg-muted py-0.5 pr-0.5 pl-2 text-xs"
          >
            <span className="min-w-0 truncate text-muted-foreground">{label}:</span>
            <span className="min-w-0 truncate font-medium">{criteria[field]}</span>
            <Button
              variant="ghost"
              size="icon"
              className="shrink-0"
              aria-label={t('search.removeCriterion', { label })}
              onClick={() => onRemove(field)}
            >
              <X aria-hidden="true" />
            </Button>
          </li>
        )
      })}
    </ul>
  )
}

/**
 * Global search (`/search`). Criteria live in the address bar; results are components or files,
 * paged by keyset, with the detail drawer for a hit. The server caps a query at 1000 hits and
 * says so (`truncated`), refuses a query without a criterion and a wildcard that is too short:
 * those two are shown next to the form, not as a failed list.
 */
export function SearchPage() {
  const search = useSearch({ from: '/_app/search' })
  const navigate = useNavigate()
  const { pageSize, setPageSize } = useTablePrefs('search')
  const [selection, setSelection] = useState<Selection | null>(null)

  const criteria = useMemo<SearchCriteria>(
    () => ({
      kind: search.kind === 'assets' ? 'assets' : 'components',
      q: search.q,
      format: search.format,
      repository: search.repository,
      group: search.group,
      name: search.name,
      version: search.version,
      checksum: search.checksum,
      path: search.path,
      sort: search.sort,
    }),
    [search],
  )
  const ready = hasCriteria(criteria)
  const components = useInfinitePage<ComponentView>(componentSearchConfig(criteria, pageSize))
  const assets = useInfinitePage<AssetView>(assetSearchConfig(criteria, pageSize))
  const active = criteria.kind === 'assets' ? assets : components

  const go = useCallback(
    (next: SearchCriteria, replace = false) => {
      void navigate({
        to: '/search',
        replace,
        search: { ...next, kind: next.kind === 'assets' ? 'assets' : undefined },
      })
    },
    [navigate],
  )
  const open = useCallback((s: Selection) => setSelection(s), [])
  const cColumns = useMemo(() => componentColumns(open), [open])
  const aColumns = useMemo(() => assetColumns(open), [open])

  const error = active.isError ? active.error : undefined
  const queryProblem =
    isApiError(error) && (error.code === 'query_too_short' || error.code === 'invalid_query')
      ? error
      : null
  const truncated =
    (active.query.data?.pages.at(-1) as { truncated?: boolean } | undefined)?.truncated === true
  const minPrefix =
    queryProblem?.code === 'query_too_short' && typeof queryProblem.details?.min_prefix === 'number'
      ? queryProblem.details.min_prefix
      : 3

  return (
    <>
      <PageHeader title={t('nav.items.search')} description={t('search.description')} />
      <div className="space-y-4">
        <CriteriaForm
          criteria={criteria}
          keywordHint={
            queryProblem?.code === 'query_too_short'
              ? t('search.tooShort', { min: minPrefix })
              : undefined
          }
          onSearch={(next) => go(next)}
          onReset={() => go({ kind: 'components' })}
        />
        <ActiveCriteria
          criteria={criteria}
          onRemove={(field) => go({ ...criteria, [field]: undefined })}
        />

        {!ready ? (
          <EmptyState
            icon={TextSearch}
            title={t('search.startTitle')}
            description={t('search.startBody')}
            action={
              <ul className="m-0 flex list-none flex-wrap justify-center gap-2 p-0">
                {EXAMPLES.map((example) => (
                  <li key={example.label}>
                    <Button asChild variant="outline" size="sm">
                      <Link to="/search" search={{ ...example.search }}>
                        <span className="font-mono">{example.label}</span>
                      </Link>
                    </Button>
                  </li>
                ))}
              </ul>
            }
          />
        ) : queryProblem ? (
          <Alert variant="warning" title={t('search.problemTitle')}>
            {queryProblem.code === 'query_too_short'
              ? t('search.tooShortBody', { min: minPrefix })
              : queryProblem.message}
          </Alert>
        ) : (
          <div className="space-y-2">
            {truncated ? <Alert variant="info">{t('search.truncated')}</Alert> : null}
            {error && !isApiError(error) ? <ErrorPanel error={error} /> : null}
            {criteria.kind === 'assets' ? (
              <DataTable<AssetView>
                tableId="search-assets"
                label={t('search.resultsAssets')}
                columns={aColumns}
                rows={assets.items}
                getRowId={(a) => a.id}
                isLoading={assets.isLoading}
                error={error}
                onRetry={assets.refetch}
                hasNextPage={assets.hasNextPage}
                isLoadingMore={assets.isLoadingMore}
                onLoadMore={assets.loadMore}
                pageSize={pageSize}
                onPageSizeChange={setPageSize}
                emptyState={<NoResults />}
              />
            ) : (
              <DataTable<ComponentView>
                tableId="search-components"
                label={t('search.resultsComponents')}
                columns={cColumns}
                rows={components.items}
                getRowId={(c) => c.id}
                isLoading={components.isLoading}
                error={error}
                onRetry={components.refetch}
                hasNextPage={components.hasNextPage}
                isLoadingMore={components.isLoadingMore}
                onLoadMore={components.loadMore}
                pageSize={pageSize}
                onPageSizeChange={setPageSize}
                emptyState={<NoResults />}
              />
            )}
          </div>
        )}
      </div>
      <DetailSheet selection={selection} onClose={() => setSelection(null)} />
    </>
  )
}

function NoResults() {
  return (
    <EmptyState
      icon={SearchX}
      title={t('search.noResultsTitle')}
      description={t('search.noResultsBody')}
    />
  )
}
