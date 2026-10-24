import { useQuery } from '@tanstack/react-query'
import { RefreshCw } from 'lucide-react'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { JobHealthWidget } from '@/features/system-dashboard/jobs-widget'
import { RequestSummaryWidget } from '@/features/system-dashboard/request-widget'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { t } from '@/i18n'
import { statusQuery, type SystemStatus } from './api'
import { ChecksCard } from './checks-card'
import { DependenciesCard } from './dependencies-card'
import { NodesCard } from './nodes-card'
import { ReadinessCard } from './readiness-card'

const OVERALL: Record<SystemStatus['status'], { kind: StatusKind; text: string }> = {
  ok: { kind: 'ok', text: 'systemStatus.overall.ok' },
  warning: { kind: 'warning', text: 'systemStatus.overall.warning' },
  error: { kind: 'error', text: 'systemStatus.overall.error' },
  unknown: { kind: 'unknown', text: 'systemStatus.overall.unknown' },
}

/**
 * `/admin/system/status`. Two separate questions, kept apart on purpose: "can this node serve
 * traffic" (readiness, from the readiness probe) and "how are things around it" (dependency
 * health and the other checks, informational). Then the nodes and the live counters.
 */
export function StatusPage() {
  const query = useQuery(statusQuery)
  const showSkeleton = useDelayedFlag(query.isPending, 150)
  const status = query.data
  const overall = status ? OVERALL[status.status] : undefined

  return (
    <>
      <PageHeader
        title={t('systemStatus.title')}
        description={
          status ? (
            <span className="flex flex-wrap items-center gap-2">
              {overall ? (
                <StatusBadge
                  status={overall.kind}
                  label={t(overall.text as 'systemStatus.overall.ok')}
                />
              ) : null}
              <span className="text-xs">
                {t('systemStatus.updated', { node: status.node })}{' '}
                <RelativeTime value={status.generated_at} />
              </span>
            </span>
          ) : (
            t('systemStatus.description')
          )
        }
        actions={
          <Button variant="outline" loading={query.isFetching} onClick={() => void query.refetch()}>
            <RefreshCw aria-hidden="true" />
            {query.isFetching ? t('systemStatus.refreshing') : t('systemStatus.refresh')}
          </Button>
        }
      />
      {query.isError && !status ? (
        <ErrorPanel
          error={query.error}
          onRetry={() => void query.refetch()}
          title={t('systemStatus.loadFailed')}
        />
      ) : status ? (
        <div className="space-y-4">
          {query.isError ? (
            <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
          ) : null}
          <div className="grid items-start gap-4 xl:grid-cols-2">
            <ReadinessCard readiness={status.readiness} />
            <DependenciesCard checks={status.checks} />
          </div>
          <ChecksCard checks={status.checks} />
          <NodesCard currentNode={status.node} />
          <section aria-labelledby="live-heading" className="space-y-2">
            <h2 id="live-heading" className="text-base font-semibold">
              {t('systemStatus.live.title')}
            </h2>
            <div className="grid items-start gap-4 xl:grid-cols-2">
              <RequestSummaryWidget />
              <JobHealthWidget />
            </div>
          </section>
        </div>
      ) : (
        <div aria-busy="true" className="min-h-48 space-y-4">
          {showSkeleton ? (
            <>
              <Skeleton className="h-40" />
              <Skeleton className="h-40" />
            </>
          ) : null}
        </div>
      )}
    </>
  )
}
