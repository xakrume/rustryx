import { TimeSeriesChart } from '@/components/charts/chart'
import { formatBytes } from '@/lib/format'
import { DemoSection } from '../demo-section'

const DAYS = 14
const GIB = 1024 ** 3

const data = Array.from({ length: DAYS }, (_, i) => {
  const day = new Date(Date.UTC(2026, 9, 10 + i)).toISOString().slice(0, 10)
  return {
    x: day,
    default: (310 + i * 7 + (i % 3) * 4) * GIB,
    archive: (92 + i * 1.5) * GIB,
    proxy: (140 - i * 1.2 + (i % 4) * 3) * GIB,
  }
})

export function ChartSection() {
  return (
    <DemoSection
      id="charts"
      title="Charts"
      description="Distinct dash patterns and marker shapes, names at the line ends, and a table behind the chart."
    >
      <TimeSeriesChart
        title="Storage used by blob store"
        data={data}
        series={[
          { key: 'default', label: 'default' },
          { key: 'archive', label: 'archive-s3' },
          { key: 'proxy', label: 'proxy-cache' },
        ]}
        valueFormatter={(v) => formatBytes(v)}
        kind="line"
      />
    </DemoSection>
  )
}
