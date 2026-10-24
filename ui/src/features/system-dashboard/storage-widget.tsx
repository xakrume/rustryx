import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BytesText } from '@/components/bytes-text'
import { TimeSeriesChart, type ChartRow, type ChartSeries } from '@/components/charts/chart'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { fetchBlobStorePage, type BlobStoreView } from '@/features/blob-stores/api'
import { UsageBar } from '@/features/blob-stores/usage-bar'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import { WidgetCard } from './widget-card'

/** One sample of a store's used space. */
export interface UsagePoint {
  at: string
  used_bytes: number
}

/**
 * `usage.history` is NOT in the contract today: the API reports current usage only, and the
 * contract allows servers to add response fields. When a server sends oldest-first points the
 * widget draws the trend; without them it shows the current state as a table, never an invented line.
 */
type UsageWithHistory = NonNullable<BlobStoreView['usage']> & { history?: readonly UsagePoint[] }

const MAX_STORES = 6
const STORE_SCAN_LIMIT = 50

const HEALTH_KIND: Record<string, StatusKind> = {
  ok: 'ok',
  degraded: 'warning',
  unreachable: 'error',
  unknown: 'unknown',
}

export function historyOf(store: BlobStoreView): readonly UsagePoint[] {
  const usage: UsageWithHistory | null = store.usage
  return usage?.history ?? []
}

/** Rows for the chart: one row per instant, one column per store (s0, s1, ...), gaps are null. */
export function buildTrend(stores: readonly BlobStoreView[]): {
  rows: ChartRow[]
  series: ChartSeries[]
} {
  const withHistory = stores.filter((s) => historyOf(s).length >= 2).slice(0, MAX_STORES)
  const instants = [...new Set(withHistory.flatMap((s) => historyOf(s).map((p) => p.at)))].sort()
  const rows = instants.map((x) => ({
    x,
    ...Object.fromEntries(
      withHistory.map((store, i) => [
        `s${i}`,
        historyOf(store).find((p) => p.at === x)?.used_bytes ?? null,
      ]),
    ),
  }))
  return { rows, series: withHistory.map((s, i) => ({ key: `s${i}`, label: s.name })) }
}

function CurrentUsage({ stores }: { stores: readonly BlobStoreView[] }) {
  return (
    <div
      role="region"
      aria-label={t('systemWidgets.storage.tableLabel')}
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll the table
      tabIndex={0}
      className="overflow-x-auto"
    >
      <table className="w-full text-left text-(length:--text-table)">
        <thead>
          <tr className="border-b border-border text-muted-foreground">
            <th scope="col" className="py-1 pr-3 font-medium">
              {t('systemWidgets.storage.columns.store')}
            </th>
            <th scope="col" className="py-1 pr-3 text-right font-medium">
              {t('systemWidgets.storage.columns.used')}
            </th>
            <th scope="col" className="py-1 pr-3 text-right font-medium">
              {t('systemWidgets.storage.columns.limit')}
            </th>
            <th scope="col" className="py-1 pr-3 font-medium">
              {t('systemWidgets.storage.columns.share')}
            </th>
            <th scope="col" className="py-1 font-medium">
              {t('systemWidgets.storage.columns.health')}
            </th>
          </tr>
        </thead>
        <tbody>
          {stores.map((store) => (
            <tr key={store.id} className="border-b border-border last:border-0">
              <th scope="row" className="py-1.5 pr-3 font-mono font-normal">
                <Link
                  to="/admin/blob-stores/$name"
                  params={{ name: store.name }}
                  className="text-link underline-offset-2 hover:underline"
                >
                  {store.name}
                </Link>
              </th>
              <td className="py-1.5 pr-3 text-right">
                <BytesText bytes={store.usage?.used_bytes} />
              </td>
              <td className="py-1.5 pr-3 text-right">
                {store.quota_bytes === null ? (
                  <span className="text-muted-foreground">
                    {t('systemWidgets.storage.noQuota')}
                  </span>
                ) : (
                  <BytesText bytes={store.quota_bytes} />
                )}
              </td>
              <td className="py-1.5 pr-3">
                <UsageBar view={store} className="min-w-40" />
              </td>
              <td className="py-1.5">
                <StatusBadge
                  status={HEALTH_KIND[store.health.state] ?? 'unknown'}
                  label={t(`blobStores.health.${store.health.state}`)}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/**
 * Storage usage of the blob stores: a trend chart when the server reports history (with the
 * chart's own data table one click away and in the DOM for screen readers), and always the
 * current usage as a real table with quota and health. Place it on the dashboard for administrators.
 */
export function StorageTrendWidget({
  className,
  compact = false,
}: {
  className?: string
  /**
   * For a page that already lists the stores (the blob store list): only the trend, and nothing
   * at all while it is loading, failing or unavailable, because that page shows those states itself.
   */
  compact?: boolean
}) {
  const query = useQuery({
    queryKey: ['system-widgets', 'storage'],
    queryFn: ({ signal }) =>
      fetchBlobStorePage({ q: undefined, kind: undefined }, null, STORE_SCAN_LIMIT, signal),
    staleTime: 60_000,
  })
  const all = query.data?.items ?? []
  const largest = [...all]
    .sort((a, b) => (b.usage?.used_bytes ?? 0) - (a.usage?.used_bytes ?? 0))
    .slice(0, MAX_STORES)
  const trend = buildTrend(largest)
  const days = trend.rows.length
  if (compact && (query.isPending || query.isError || trend.series.length === 0)) return null
  return (
    <WidgetCard
      title={t('systemWidgets.storage.title')}
      description={t('systemWidgets.storage.description')}
      loading={query.isPending}
      error={query.error}
      onRetry={() => void query.refetch()}
      hasData={query.data !== undefined}
      className={className}
      actions={
        compact ? undefined : (
          <Link to="/admin/blob-stores" className="text-sm text-link underline underline-offset-2">
            {t('systemWidgets.storage.manage')}
          </Link>
        )
      }
    >
      {compact ? (
        <TimeSeriesChart
          title={t('systemWidgets.storage.chartTitle')}
          data={trend.rows}
          series={trend.series}
          kind="line"
          valueFormatter={(value) => formatBytes(value)}
          height={200}
        />
      ) : all.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t('systemWidgets.storage.empty')}</p>
      ) : (
        <div className="space-y-3">
          {trend.series.length > 0 ? (
            <TimeSeriesChart
              title={t('systemWidgets.storage.chartTitle')}
              data={trend.rows}
              series={trend.series}
              kind="line"
              valueFormatter={(value) => formatBytes(value)}
              height={200}
            />
          ) : (
            <p className="text-sm text-muted-foreground">{t('systemWidgets.storage.noHistory')}</p>
          )}
          {days > 0 ? (
            <p className="text-xs text-muted-foreground">
              {t('systemWidgets.storage.trendDays', { count: days })}
            </p>
          ) : null}
          <CurrentUsage stores={largest} />
          {all.length > MAX_STORES ? (
            <p className="text-xs text-muted-foreground">
              {t('systemWidgets.storage.moreStores', { count: MAX_STORES, total: all.length })}
            </p>
          ) : null}
        </div>
      )}
    </WidgetCard>
  )
}
