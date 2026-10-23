import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Check, Circle } from 'lucide-react'
import { BytesText } from '@/components/bytes-text'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Alert } from '@/components/ui/alert'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { t } from '@/i18n'
import { dashboardQuery, type DashboardSummary } from './api'

const HEALTH_STATUS: Record<string, StatusKind> = {
  ok: 'ok',
  degraded: 'warning',
  unreachable: 'error',
  unknown: 'unknown',
}

function CountList({ counts }: { counts: Record<string, number> }) {
  return (
    <ul className="space-y-1 text-sm">
      {Object.entries(counts)
        .sort(([, a], [, b]) => b - a)
        .map(([name, count]) => (
          <li key={name} className="flex justify-between gap-4">
            <span className="font-mono">{name}</span>
            <span className="tabular">{count}</span>
          </li>
        ))}
    </ul>
  )
}

function Summary({ summary }: { summary: DashboardSummary }) {
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
      <Card>
        <CardHeader>
          <CardTitle>{t('dashboard.repositories')}</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <p className="text-2xl font-semibold tabular">
            {t('dashboard.repositoriesTotal', { count: summary.repositories.total })}
          </p>
          {summary.repositories.offline > 0 ? (
            <StatusBadge
              status="warning"
              label={`${t('dashboard.offline')}: ${summary.repositories.offline}`}
            />
          ) : null}
          <div className="grid grid-cols-2 gap-4">
            <div>
              <h3 className="mb-1 text-xs font-medium text-muted-foreground">
                {t('dashboard.byFormat')}
              </h3>
              <CountList counts={summary.repositories.by_format} />
            </div>
            <div>
              <h3 className="mb-1 text-xs font-medium text-muted-foreground">
                {t('dashboard.byType')}
              </h3>
              <CountList counts={summary.repositories.by_type} />
            </div>
          </div>
        </CardContent>
      </Card>

      {summary.storage ? (
        <Card>
          <CardHeader>
            <CardTitle>{t('dashboard.storage')}</CardTitle>
          </CardHeader>
          <CardContent>
            <ul className="space-y-3">
              {summary.storage.map((store) => (
                <li key={store.blob_store} className="space-y-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-mono text-sm">{store.blob_store}</span>
                    <StatusBadge
                      status={HEALTH_STATUS[store.health] ?? 'unknown'}
                      label={t(`dashboard.health.${store.health}`)}
                    />
                  </div>
                  <p className="text-sm text-muted-foreground">
                    <BytesText bytes={store.used_bytes} />{' '}
                    {store.quota_bytes === null ? (
                      t('dashboard.noQuota')
                    ) : (
                      <>
                        {t('dashboard.storageQuota', { quota: '' })}
                        <BytesText bytes={store.quota_bytes} />
                      </>
                    )}
                  </p>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      ) : null}

      {summary.setup_checklist && summary.setup_checklist.some((step) => !step.done) ? (
        <Card>
          <CardHeader>
            <CardTitle>{t('dashboard.setup')}</CardTitle>
          </CardHeader>
          <CardContent>
            <ul className="space-y-2 text-sm">
              {summary.setup_checklist.map((step) => (
                <li key={step.id} className="flex items-center gap-2">
                  {step.done ? (
                    <Check className="size-4 text-success-text" aria-hidden="true" />
                  ) : (
                    <Circle className="size-4 text-muted-foreground" aria-hidden="true" />
                  )}
                  {/* The status is text as well as an icon: "Done" or "To do" for screen readers. */}
                  <span className="sr-only">
                    {step.done ? t('dashboard.done') : t('dashboard.todo')}:{' '}
                  </span>
                  {step.done ? (
                    <span>{t(`dashboard.checklist.${step.id}`)}</span>
                  ) : (
                    <Link to={step.link} className="text-link underline underline-offset-2">
                      {t(`dashboard.checklist.${step.id}`)}
                    </Link>
                  )}
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      ) : null}

      {summary.recent_activity ? (
        <Card className="md:col-span-2 xl:col-span-3">
          <CardHeader>
            <CardTitle>{t('dashboard.recentActivity')}</CardTitle>
          </CardHeader>
          <CardContent>
            {summary.recent_activity.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('dashboard.noActivity')}</p>
            ) : (
              <ul className="divide-y divide-border text-sm">
                {summary.recent_activity.map((event) => (
                  <li key={event.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                    <RelativeTime value={event.at} className="w-28 text-muted-foreground" />
                    <span className="font-mono">{event.action}</span>
                    <span className="text-muted-foreground">{event.actor_name ?? '-'}</span>
                    {event.target ? <span className="font-mono">{event.target}</span> : null}
                    <StatusBadge
                      status={event.outcome === 'success' ? 'ok' : 'error'}
                      label={event.outcome}
                      className="ml-auto"
                    />
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>
      ) : null}
    </div>
  )
}

export function DashboardPage() {
  const { data, isPending, error, refetch } = useQuery(dashboardQuery)
  const showSkeleton = useDelayedFlag(isPending, 150)
  return (
    <>
      <PageHeader title={t('pages.home')} description={t('dashboard.description')} />
      {data?.public_only ? (
        <Alert variant="info" className="mb-4">
          {t('dashboard.publicOnly')}
        </Alert>
      ) : null}
      {error && !data ? (
        <ErrorPanel
          error={error}
          onRetry={() => void refetch()}
          title={t('dashboard.loadFailed')}
        />
      ) : data ? (
        <Summary summary={data} />
      ) : (
        <div aria-busy="true" className="grid min-h-48 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {showSkeleton ? (
            <>
              <Skeleton className="h-48" />
              <Skeleton className="h-48" />
              <Skeleton className="h-48" />
            </>
          ) : null}
        </div>
      )}
    </>
  )
}
