import { useId, useState } from 'react'
import {
  Area,
  AreaChart,
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Button } from '@/components/ui/button'
import { useCssColors } from '@/hooks/use-css-colors'
import { usePrefersReducedMotion } from '@/hooks/use-media-query'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

export interface ChartSeries {
  /** Key of the value in each data row. */
  key: string
  /** Name shown in the direct label, the tooltip and the table header. */
  label: string
}

export type ChartRow = Readonly<Record<string, number | string | null>>

export interface TimeSeriesChartProps {
  /** Accessible name and table caption: "Storage used by blob store". */
  title: string
  data: readonly ChartRow[]
  series: readonly ChartSeries[]
  /** Key of the x value in each row. Default "x". */
  xKey?: string
  kind?: 'line' | 'area'
  valueFormatter?: (value: number) => string
  xFormatter?: (x: string) => string
  height?: number
  className?: string
}

/** Rule 6 of the design brief: no more than six series. */
export const MAX_SERIES = 6

/** Dash patterns that differ without color: solid, dashed, dotted, dash-dot, sparse dots, long dash. */
const DASHES = ['0', '7 4', '2 4', '10 4 2 4', '1 6', '14 5'] as const
const SHAPES = ['circle', 'square', 'triangle', 'diamond', 'cross', 'ring'] as const
const SERIES_TOKENS = [1, 2, 3, 4, 5, 6].map((n) => `--color-chart-${n}`)
const THEME_TOKENS = ['--color-chart-grid', '--color-muted-foreground', '--color-card']

interface MarkerProps {
  cx?: number
  cy?: number
  shape: (typeof SHAPES)[number]
  color: string
  /** Fill of the hollow marker: the card color, resolved (see useCssColors). */
  hollow: string
}

/** A point marker whose SHAPE differs per series, a second cue next to color and dash. */
function Marker({ cx, cy, shape, color, hollow }: MarkerProps) {
  if (cx === undefined || cy === undefined) return null
  const r = 4
  switch (shape) {
    case 'square':
      return <rect x={cx - r} y={cy - r} width={2 * r} height={2 * r} fill={color} />
    case 'triangle':
      return (
        <path
          d={`M${cx} ${cy - r - 1} L${cx + r + 1} ${cy + r} L${cx - r - 1} ${cy + r} Z`}
          fill={color}
        />
      )
    case 'diamond':
      return (
        <path
          d={`M${cx} ${cy - r - 1} L${cx + r + 1} ${cy} L${cx} ${cy + r + 1} L${cx - r - 1} ${cy} Z`}
          fill={color}
        />
      )
    case 'cross':
      return (
        <path
          d={`M${cx - r} ${cy} H${cx + r} M${cx} ${cy - r} V${cy + r}`}
          stroke={color}
          strokeWidth={2}
          fill="none"
        />
      )
    case 'ring':
      return <circle cx={cx} cy={cy} r={r - 0.5} fill={hollow} stroke={color} strokeWidth={2} />
    default:
      return <circle cx={cx} cy={cy} r={r - 0.5} fill={color} />
  }
}

function defaultXFormatter(x: string): string {
  const date = new Date(x)
  if (Number.isNaN(date.getTime())) return x
  return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(date)
}

/**
 * Pushes direct labels apart. Series that end at similar values would print their names on top
 * of each other; each label asks for its y and gets the nearest free position at least
 * `minGap` pixels from the labels placed before it. One placer lives for one render of the
 * chart, so the result does not depend on earlier renders.
 */
export function createLabelPlacer(minGap: number): (y: number) => number {
  const placed: number[] = []
  return (y) => {
    let candidate = y
    for (let guard = 0; guard < 20; guard += 1) {
      const clash = placed.find((p) => Math.abs(p - candidate) < minGap)
      if (clash === undefined) break
      candidate = clash + minGap
    }
    placed.push(candidate)
    return candidate
  }
}

const LABEL_GAP = 14

/** Only the last point carries the series name, at the line end ("direct label"). */
function endLabel(color: string, text: string, lastIndex: number, place: (y: number) => number) {
  return function EndLabel(props: { x?: string | number; y?: string | number; index?: number }) {
    if (props.index !== lastIndex || props.x === undefined || props.y === undefined) return null
    return (
      <text
        x={Number(props.x) + 8}
        y={place(Number(props.y))}
        dy={4}
        fontSize={12}
        fontWeight={600}
        fill={color}
      >
        {text}
      </text>
    )
  }
}

/**
 * Line or area chart that does not rely on color: every series has its own dash pattern and
 * marker shape, its name is printed at the end of its line (no legend to decode), and the exact
 * numbers are one click away in a real table. Screen readers get that table directly, visually
 * hidden, because an SVG is not navigable data. Animation is off under prefers-reduced-motion.
 */
export function TimeSeriesChart({
  title,
  data,
  series: allSeries,
  xKey = 'x',
  kind = 'line',
  valueFormatter = (v) => String(v),
  xFormatter = defaultXFormatter,
  height = 240,
  className,
}: TimeSeriesChartProps) {
  const [showTable, setShowTable] = useState(false)
  // Resolved colors: SVG attributes cannot use var(--token), see useCssColors.
  const seriesColors = useCssColors(SERIES_TOKENS)
  const [gridColor = 'currentColor', mutedColor = 'currentColor', cardColor = 'transparent'] =
    useCssColors(THEME_TOKENS)
  const placeLabel = createLabelPlacer(LABEL_GAP)
  const reduced = usePrefersReducedMotion()
  const captionId = useId()
  const series = allSeries.slice(0, MAX_SERIES)
  const truncated = allSeries.length > MAX_SERIES
  const rows = data.map((row) => ({ ...row }))
  const lastIndex = rows.length - 1

  const table = (
    <table
      className="w-full border-collapse text-left text-(length:--text-table)"
      aria-labelledby={captionId}
    >
      <caption id={captionId} className="py-1 text-left text-sm font-medium">
        {t('charts.tableCaption', { title })}
      </caption>
      <thead>
        <tr className="border-b border-border">
          <th scope="col" className="px-2 py-1 font-medium text-muted-foreground">
            {t('charts.seriesColumn')}
          </th>
          {series.map((s) => (
            <th
              key={s.key}
              scope="col"
              className="px-2 py-1 text-right font-medium text-muted-foreground"
            >
              {s.label}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((row, i) => (
          <tr key={`${String(row[xKey])}-${i}`} className="border-b border-border">
            <th scope="row" className="px-2 py-1 font-normal">
              {xFormatter(String(row[xKey]))}
            </th>
            {series.map((s) => {
              const value = row[s.key]
              return (
                <td key={s.key} className="px-2 py-1 text-right tabular-nums">
                  {typeof value === 'number' ? valueFormatter(value) : '-'}
                </td>
              )
            })}
          </tr>
        ))}
      </tbody>
    </table>
  )

  const common = (
    <>
      <CartesianGrid stroke={gridColor} strokeDasharray="3 3" vertical={false} />
      <XAxis
        dataKey={xKey}
        tickFormatter={xFormatter}
        stroke={mutedColor}
        tick={{ fill: mutedColor, fontSize: 12 }}
        tickLine={false}
      />
      <YAxis
        tickFormatter={valueFormatter}
        stroke={mutedColor}
        tick={{ fill: mutedColor, fontSize: 12 }}
        tickLine={false}
        width={64}
      />
      <Tooltip
        formatter={(value) => (typeof value === 'number' ? valueFormatter(value) : String(value))}
        labelFormatter={(label) =>
          typeof label === 'string' || typeof label === 'number' ? xFormatter(String(label)) : ''
        }
        contentStyle={{
          background: 'var(--color-popover)',
          border: '1px solid var(--color-border)',
          borderRadius: 8,
          color: 'var(--color-popover-foreground)',
          fontSize: 12,
        }}
        itemStyle={{ color: 'var(--color-popover-foreground)' }}
        labelStyle={{ color: 'var(--color-muted-foreground)' }}
      />
    </>
  )

  const margin = { top: 8, right: 96, bottom: 4, left: 0 }
  const showDots = rows.length <= 31

  return (
    <figure
      className={cn(
        'rounded-xl border border-transparent bg-card dark:border-border/60 p-3',
        className,
      )}
    >
      <figcaption className="mb-2 flex items-center justify-between gap-2">
        <span className="text-sm font-semibold">{title}</span>
        <Button
          variant="outline"
          size="sm"
          aria-pressed={showTable}
          onClick={() => setShowTable((v) => !v)}
        >
          {showTable ? t('charts.showChart') : t('charts.showTable')}
        </Button>
      </figcaption>
      {rows.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">{t('charts.noData')}</p>
      ) : showTable ? (
        <div className="overflow-x-auto">{table}</div>
      ) : (
        <>
          {/* The drawing is hidden from assistive tech (the table is its text version), so
              Recharts' own keyboard layer is off below: a focusable element inside aria-hidden
              is a WCAG violation (aria-hidden-focus). */}
          <div aria-hidden="true" style={{ height }}>
            <ResponsiveContainer width="100%" height="100%">
              {kind === 'area' ? (
                <AreaChart data={rows} margin={margin} accessibilityLayer={false}>
                  {common}
                  {series.map((s, i) => (
                    <Area
                      key={s.key}
                      type="monotone"
                      dataKey={s.key}
                      name={s.label}
                      stroke={seriesColors[i] ?? 'currentColor'}
                      strokeWidth={2}
                      strokeDasharray={DASHES[i]}
                      fill={seriesColors[i] ?? 'currentColor'}
                      fillOpacity={0.12}
                      isAnimationActive={!reduced}
                      dot={
                        showDots ? (
                          <Marker
                            shape={SHAPES[i] ?? 'circle'}
                            color={seriesColors[i] ?? 'currentColor'}
                            hollow={cardColor}
                          />
                        ) : (
                          false
                        )
                      }
                      label={endLabel(
                        seriesColors[i] ?? 'currentColor',
                        s.label,
                        lastIndex,
                        placeLabel,
                      )}
                    />
                  ))}
                </AreaChart>
              ) : (
                <LineChart data={rows} margin={margin} accessibilityLayer={false}>
                  {common}
                  {series.map((s, i) => (
                    <Line
                      key={s.key}
                      type="monotone"
                      dataKey={s.key}
                      name={s.label}
                      stroke={seriesColors[i] ?? 'currentColor'}
                      strokeWidth={2}
                      strokeDasharray={DASHES[i]}
                      isAnimationActive={!reduced}
                      dot={
                        showDots ? (
                          <Marker
                            shape={SHAPES[i] ?? 'circle'}
                            color={seriesColors[i] ?? 'currentColor'}
                            hollow={cardColor}
                          />
                        ) : (
                          false
                        )
                      }
                      label={endLabel(
                        seriesColors[i] ?? 'currentColor',
                        s.label,
                        lastIndex,
                        placeLabel,
                      )}
                    />
                  ))}
                </LineChart>
              )}
            </ResponsiveContainer>
          </div>
          {/* Visually hidden but present for screen readers. The wrapper carries sr-only: on the
              table itself it fought the table's own width class and let the table stick out of
              the page on narrow screens. */}
          <div className="sr-only">{table}</div>
        </>
      )}
      {truncated ? (
        <p className="mt-1 text-xs text-muted-foreground">{t('charts.maxSeries')}</p>
      ) : null}
    </figure>
  )
}
