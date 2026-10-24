import { useQuery } from '@tanstack/react-query'
import { StatusBadge } from '@/components/status-badge'
import { metricsQuery, type MetricsSummary } from '@/features/system/api'
import { t } from '@/i18n'
import { formatUptime } from '@/features/system/uptime'
import { WidgetCard } from './widget-card'

/** Share of server errors from which the summary says "above 1 percent". */
const ERROR_RATE_ALERT = 0.01

function Tile({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-muted px-3 py-2">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 text-xl font-semibold tabular-nums">{children}</dd>
    </div>
  )
}

export function RequestSummary({ metrics }: { metrics: MetricsSummary }) {
  const { http, auth } = metrics
  const rate = http.requests_total === 0 ? 0 : http.errors_5xx_total / http.requests_total
  const high = rate >= ERROR_RATE_ALERT
  return (
    <div className="space-y-3">
      <dl
        aria-label={t('systemWidgets.requests.tilesLabel')}
        className="grid grid-cols-2 gap-2 sm:grid-cols-3"
      >
        <Tile label={t('systemWidgets.requests.requests')}>
          {http.requests_total.toLocaleString()}
        </Tile>
        <Tile label={t('systemWidgets.requests.errors')}>
          {http.errors_5xx_total.toLocaleString()}
        </Tile>
        <Tile label={t('systemWidgets.requests.errorRate')}>
          <span className="flex flex-wrap items-center gap-2">
            {(rate * 100).toFixed(2)}%
            <StatusBadge
              status={high ? 'warning' : 'ok'}
              label={t(high ? 'systemWidgets.requests.rateHigh' : 'systemWidgets.requests.rateOk')}
            />
          </span>
        </Tile>
        <Tile label={t('systemWidgets.requests.p95')}>
          {http.p95_ms === null
            ? t('systemWidgets.requests.unknown')
            : `${Math.round(http.p95_ms)} ms`}
        </Tile>
        <Tile label={t('systemWidgets.requests.inFlight')}>{http.in_flight}</Tile>
        <Tile label={t('systemWidgets.requests.shed')}>{http.shed_total.toLocaleString()}</Tile>
        <Tile label={t('systemWidgets.requests.loginsFailed')}>
          {auth.logins_failed_total.toLocaleString()}
        </Tile>
        <Tile label={t('systemWidgets.requests.lockouts')}>
          {auth.lockouts_total.toLocaleString()}
        </Tile>
      </dl>
      <p className="text-xs text-muted-foreground">
        {t('systemWidgets.requests.scope', {
          node: metrics.node,
          uptime: formatUptime(metrics.uptime_secs),
        })}
      </p>
    </div>
  )
}

/** Request and error counters of the answering node (`GET /system/metrics/summary`). */
export function RequestSummaryWidget({ className }: { className?: string }) {
  const query = useQuery(metricsQuery)
  return (
    <WidgetCard
      title={t('systemWidgets.requests.title')}
      description={t('systemWidgets.requests.description')}
      loading={query.isPending}
      error={query.error}
      onRetry={() => void query.refetch()}
      hasData={query.data !== undefined}
      className={className}
    >
      {query.data ? <RequestSummary metrics={query.data} /> : null}
    </WidgetCard>
  )
}
