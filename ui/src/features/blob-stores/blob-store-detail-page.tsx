import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowLeft } from 'lucide-react'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { t } from '@/i18n'
import { blobStoreQuery, blobStoreRepositoriesQuery } from './api'
import { ConfigForm } from './blob-store-config-form'
import { ConnectionCard, DeleteCard, RepositoriesCard, UsageCard } from './blob-store-sections'
import { HealthBadge } from './usage-bar'

/** `/admin/blob-stores/:name`: usage, configuration, connection test, repositories, delete. */
export function BlobStoreDetailPage({ name }: { name: string }) {
  const query = useQuery(blobStoreQuery(name))
  const repos = useQuery(blobStoreRepositoriesQuery(name))
  const showSkeleton = useDelayedFlag(query.isPending, 150)
  const stored = query.data

  return (
    <>
      <PageHeader
        title={name}
        description={
          stored ? (
            <span className="flex flex-wrap items-center gap-2">
              <Badge>{t(`blobStores.kind.${stored.data.kind}`)}</Badge>
              <HealthBadge health={stored.data.health} />
              {stored.data.health.last_error_code ? (
                <span className="font-mono text-xs">
                  {t('blobStores.health.lastError', { code: stored.data.health.last_error_code })}
                </span>
              ) : null}
              {stored.data.health.checked_at ? (
                <span className="text-xs">
                  {t('blobStores.health.checkedAt', { time: '' })}
                  <RelativeTime value={stored.data.health.checked_at} />
                </span>
              ) : null}
            </span>
          ) : undefined
        }
        actions={
          <Button asChild variant="outline">
            <Link to="/admin/blob-stores">
              <ArrowLeft aria-hidden="true" />
              {t('blobStores.backToList')}
            </Link>
          </Button>
        }
      />
      {query.isError && !stored ? (
        <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
      ) : stored ? (
        <div className="grid items-start gap-4 xl:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
          <div className="space-y-4">
            {/* Keyed by the ETag: a reload or a save that moved the version rebuilds the form
                from the server's object, so the form can never show a stale base. */}
            <ConfigForm key={stored.etag ?? stored.data.id} view={stored.data} />
          </div>
          <div className="space-y-4">
            <UsageCard view={stored.data} />
            <ConnectionCard name={name} />
            <RepositoriesCard view={stored.data} />
            <DeleteCard
              view={stored.data}
              etag={stored.etag}
              repositoryNames={repos.data?.items.map((r) => r.name) ?? []}
            />
          </div>
        </div>
      ) : (
        <div aria-busy="true" className="min-h-48 space-y-4">
          {showSkeleton ? (
            <>
              <Skeleton className="h-48" />
              <Skeleton className="h-32" />
            </>
          ) : null}
        </div>
      )}
    </>
  )
}
