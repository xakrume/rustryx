import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { ArrowLeft } from 'lucide-react'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { getCsrfToken } from '@/api/csrf'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { blobStoreQuery, blobStoreRepositoriesQuery } from './api'
import { ConfigForm } from './blob-store-config-form'
import { ConnectionCard, DeleteCard, RepositoriesCard, UsageCard } from './blob-store-sections'
import { HealthBadge } from './usage-bar'

type DedupView = {
  status: string
  links: number
  bytes_saved: number
  errors: number
  error: string | null
}

async function readDedup(name: string, method: 'GET' | 'POST'): Promise<DedupView> {
  const headers = new Headers({ Accept: 'application/json' })
  if (method === 'POST') {
    const csrf = getCsrfToken()
    if (csrf) headers.set('x-csrf-token', csrf)
  }
  const response = await fetch(`/api/v1/blob-stores/${encodeURIComponent(name)}/actions/dedup`, {
    method,
    credentials: 'include',
    headers,
  })
  if (!response.ok) throw new Error(String(response.status))
  return (await response.json()) as DedupView
}

function DedupCard({ name }: { name: string }) {
  const [view, setView] = useState<DedupView | null>(null)
  useEffect(() => {
    let stop = false
    const tick = () => {
      readDedup(name, 'GET')
        .then((next) => {
          if (!stop) setView(next)
        })
        .catch(() => undefined)
    }
    tick()
    const timer = window.setInterval(tick, 2000)
    return () => {
      stop = true
      window.clearInterval(timer)
    }
  }, [name])
  return (
    <section className="space-y-3 rounded-xl border border-transparent bg-card dark:border-border/60 p-4">
      <h2 className="text-base font-semibold">{t('blobStores.dedup.title')}</h2>
      <p className="text-sm text-muted-foreground">{t('blobStores.dedup.body')}</p>
      <Button
        variant="outline"
        loading={view?.status === 'running'}
        onClick={() => {
          readDedup(name, 'POST')
            .then(setView)
            .catch(() => toast.apiError(new Error('dedup'), t('blobStores.dedup.failed')))
        }}
      >
        {view?.status === 'running' ? t('blobStores.dedup.running') : t('blobStores.dedup.action')}
      </Button>
      {view && view.status !== 'idle' ? (
        <p className="text-sm text-muted-foreground">
          {t('blobStores.dedup.result', {
            links: view.links,
            bytes: view.bytes_saved,
            errors: view.errors,
          })}
          {view.error ? ` ${view.error}` : ''}
        </p>
      ) : null}
    </section>
  )
}

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
            {stored.data.kind === 'local' ? <DedupCard name={name} /> : null}
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
