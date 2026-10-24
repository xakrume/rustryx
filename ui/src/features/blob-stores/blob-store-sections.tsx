import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { CircleAlert, CircleCheck, RefreshCw } from 'lucide-react'
import { useState } from 'react'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { BytesText } from '@/components/bytes-text'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  blobStoreKeys,
  blobStoreRepositoriesQuery,
  blobStoreUsageQuery,
  deleteBlobStore,
  REPOSITORY_PREVIEW_LIMIT,
  testBlobStoreConnection,
  type BlobStoreTestResult,
  type BlobStoreView,
} from './api'
import { UsageBar } from './usage-bar'

export function UsageCard({ view }: { view: BlobStoreView }) {
  const usage = useQuery(blobStoreUsageQuery(view.name))
  const numbers = usage.data ?? view.usage
  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-2">
        <CardTitle>{t('blobStores.sections.usage')}</CardTitle>
        <Button
          variant="outline"
          size="sm"
          loading={usage.isFetching}
          onClick={() => void usage.refetch()}
        >
          <RefreshCw aria-hidden="true" />
          {t('blobStores.actions.refresh')}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <UsageBar view={{ ...view, usage: numbers }} />
        {numbers ? (
          <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm sm:grid-cols-4">
            <div>
              <dt className="text-muted-foreground">{t('blobStores.usageDetail.used')}</dt>
              <dd>
                <BytesText bytes={numbers.used_bytes} />
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">{t('blobStores.usageDetail.available')}</dt>
              <dd>
                <BytesText bytes={numbers.available_bytes} />
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">{t('blobStores.usageDetail.total')}</dt>
              <dd>
                <BytesText bytes={numbers.total_bytes} />
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">{t('blobStores.usageDetail.blobs')}</dt>
              <dd className="tabular-nums">{numbers.blob_count.toLocaleString()}</dd>
            </div>
          </dl>
        ) : null}
        {numbers ? (
          <p className="text-xs text-muted-foreground">
            {t('blobStores.usage.asOf', { time: '' })}
            <RelativeTime value={numbers.as_of} />
          </p>
        ) : null}
        {view.quota_bytes !== null ? (
          <p className="text-xs text-muted-foreground">{t('blobStores.quota.softNote')}</p>
        ) : null}
        {usage.isError ? (
          <ErrorPanel error={usage.error} onRetry={() => void usage.refetch()} />
        ) : null}
      </CardContent>
    </Card>
  )
}

export function ConnectionCard({ name }: { name: string }) {
  const [result, setResult] = useState<BlobStoreTestResult | null>(null)
  const [failure, setFailure] = useState<unknown>(null)
  const test = useMutation({
    mutationFn: () => testBlobStoreConnection(name),
    meta: { silent: true },
    onMutate: () => {
      setResult(null)
      setFailure(null)
    },
    onSuccess: setResult,
    onError: setFailure,
  })
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('blobStores.sections.connection')}</CardTitle>
        <CardDescription>{t('blobStores.test.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <Button variant="outline" loading={test.isPending} onClick={() => test.mutate()}>
          {test.isPending
            ? t('blobStores.actions.testing')
            : t('blobStores.actions.testConnection')}
        </Button>
        {/* One live region for both outcomes, so a screen reader hears the verdict. */}
        <div aria-live="polite">
          {result?.ok === true ? (
            <Alert variant="success" title={t('blobStores.test.ok')}>
              <span className="flex items-center gap-1">
                <CircleCheck className="size-4" aria-hidden="true" />
                {t('blobStores.test.okDetail', { ms: result.latency_ms ?? 0 })}
              </span>
            </Alert>
          ) : null}
          {result?.ok === false ? (
            <Alert variant="destructive" title={t('blobStores.test.failed')}>
              <span className="flex items-center gap-1">
                <CircleAlert className="size-4" aria-hidden="true" />
                {t('blobStores.test.failedDetail', { code: result.error_code ?? 'unknown' })}
              </span>
            </Alert>
          ) : null}
        </div>
        {failure !== null ? <ErrorPanel error={failure} /> : null}
      </CardContent>
    </Card>
  )
}

export function RepositoriesCard({ view }: { view: BlobStoreView }) {
  const repos = useQuery(blobStoreRepositoriesQuery(view.name))
  const items = repos.data?.items ?? []
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('blobStores.sections.repositories')}</CardTitle>
        <CardDescription>
          {t('blobStores.repositoriesCount', { count: view.repository_count })}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {repos.isPending ? (
          <Skeleton className="h-16" />
        ) : repos.isError ? (
          <ErrorPanel
            error={repos.error}
            onRetry={() => void repos.refetch()}
            title={t('blobStores.repos.loadFailed')}
          />
        ) : items.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('blobStores.repos.none')}</p>
        ) : (
          <>
            <ul className="flex flex-wrap gap-2">
              {items.map((repository) => (
                <li key={repository.id}>
                  <Link
                    to="/repositories/$name"
                    params={{ name: repository.name }}
                    className="inline-block rounded-md border border-border bg-muted px-2 py-0.5 font-mono text-xs text-link underline-offset-2 hover:underline"
                  >
                    {repository.name}
                  </Link>
                </li>
              ))}
            </ul>
            {view.repository_count > REPOSITORY_PREVIEW_LIMIT ? (
              <p className="text-xs text-muted-foreground">
                {t('blobStores.repos.more', { count: REPOSITORY_PREVIEW_LIMIT })}
              </p>
            ) : null}
          </>
        )}
      </CardContent>
    </Card>
  )
}

/**
 * Delete is blocked, with the reason in words and the blocking repositories named, while any
 * repository uses the store (contract: 409 `resource_in_use`). The button is disabled instead of
 * hidden (design brief rule 5). If the server still answers 409, because a repository was
 * attached after the page loaded, the confirm dialog lists the dependents it returns.
 */
export function DeleteCard({
  view,
  etag,
  repositoryNames,
}: {
  view: BlobStoreView
  etag: string | null
  repositoryNames: readonly string[]
}) {
  const [open, setOpen] = useState(false)
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const blocked = view.repository_count > 0
  const reasonId = `delete-reason-${view.id}`

  async function confirm(): Promise<void> {
    await deleteBlobStore(view.name, etag)
    await queryClient.invalidateQueries({ queryKey: blobStoreKeys.all })
    toast.success(t('blobStores.delete.done', { name: view.name }))
    void navigate({ to: '/admin/blob-stores' })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('blobStores.sections.danger')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {blocked ? (
          <Alert variant="warning" title={t('blobStores.delete.blockedTitle')}>
            <span id={reasonId}>
              {t('blobStores.delete.blocked', {
                count: view.repository_count,
                names:
                  repositoryNames.slice(0, 5).join(', ') +
                  (view.repository_count > 5 ? ', ...' : ''),
              })}
            </span>
          </Alert>
        ) : (
          <p id={reasonId} className="text-sm text-muted-foreground">
            {t('blobStores.delete.free')}
          </p>
        )}
        <Button
          variant="destructive"
          disabled={blocked}
          aria-describedby={reasonId}
          onClick={() => setOpen(true)}
        >
          {t('blobStores.actions.delete')}
        </Button>
        <ConfirmDialog
          open={open}
          onOpenChange={setOpen}
          kind="blob store"
          name={view.name}
          description={t('blobStores.delete.confirmDescription')}
          requireTyping
          onConfirm={confirm}
        />
      </CardContent>
    </Card>
  )
}
