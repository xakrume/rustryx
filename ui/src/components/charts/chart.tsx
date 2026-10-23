import { lazy, Suspense } from 'react'
import { Skeleton } from '@/components/ui/skeleton'
import type { TimeSeriesChartProps } from './time-series-chart'

export type { ChartRow, ChartSeries, TimeSeriesChartProps } from './time-series-chart'

const Lazy = lazy(() => import('./time-series-chart').then((m) => ({ default: m.TimeSeriesChart })))

/**
 * Charts are code split: Recharts and d3 are the heaviest dependency of the UI and only the
 * dashboard and usage pages draw charts, so they must not be part of the initial bundle.
 * Import `TimeSeriesChart` from here; import from './time-series-chart' only in tests.
 */
export function TimeSeriesChart(props: TimeSeriesChartProps) {
  return (
    <Suspense
      fallback={<Skeleton className="w-full" style={{ height: (props.height ?? 240) + 56 }} />}
    >
      <Lazy {...props} />
    </Suspense>
  )
}
