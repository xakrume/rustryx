import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations } from '@/test/utils'
import { createLabelPlacer, MAX_SERIES, TimeSeriesChart } from './time-series-chart'

const data = [
  { x: '2026-10-22', a: 10, b: 5 },
  { x: '2026-10-23', a: 14, b: 6 },
  { x: '2026-10-24', a: 18, b: null },
]
const series = [
  { key: 'a', label: 'default' },
  { key: 'b', label: 'archive' },
]

// jsdom has no layout, so the responsive container measures 0 by 0 and Recharts warns about it on
// every render. That is expected here (the drawing is not what these tests look at); any other
// warning still gets through.
beforeEach(() => {
  const warn = console.warn.bind(console)
  vi.spyOn(console, 'warn').mockImplementation((...args: unknown[]) => {
    if (String(args[0]).includes('width(0) and height(0)')) return
    warn(...args)
  })
})

describe('TimeSeriesChart', () => {
  it('gives screen readers the data as a real table', () => {
    render(
      <TimeSeriesChart
        title="Storage"
        data={data}
        series={series}
        valueFormatter={(v) => `${v} GiB`}
      />,
    )
    const table = screen.getByRole('table', { name: 'Storage: data table' })
    expect(table.closest('.sr-only')).not.toBeNull()
    expect(screen.getByRole('columnheader', { name: 'default' })).toBeInTheDocument()
    expect(screen.getByRole('cell', { name: '14 GiB' })).toBeInTheDocument()
    // A missing value is a dash, not a zero.
    expect(screen.getAllByRole('cell', { name: '-' })).toHaveLength(1)
  })

  it('hides the drawing from assistive technology', () => {
    const { container } = render(<TimeSeriesChart title="Storage" data={data} series={series} />)
    expect(container.querySelector('[aria-hidden="true"][style]')).not.toBeNull()
  })

  it('switches to a visible table and back', async () => {
    const user = userEvent.setup()
    render(<TimeSeriesChart title="Storage" data={data} series={series} />)
    const toggle = screen.getByRole('button', { name: 'Show data table' })
    expect(toggle).toHaveAttribute('aria-pressed', 'false')
    await user.click(toggle)
    expect(screen.getByRole('table').closest('.sr-only')).toBeNull()
    await user.click(screen.getByRole('button', { name: 'Show chart' }))
    expect(screen.getByRole('table').closest('.sr-only')).not.toBeNull()
  })

  it('must_not_draw_more_than_six_series', () => {
    const many = Array.from({ length: MAX_SERIES + 2 }, (_, i) => ({
      key: `s${i}`,
      label: `series ${i}`,
    }))
    const rows = [{ x: '2026-10-24', ...Object.fromEntries(many.map((s, i) => [s.key, i])) }]
    render(<TimeSeriesChart title="Too many" data={rows} series={many} />)
    expect(screen.getAllByRole('columnheader')).toHaveLength(MAX_SERIES + 1)
    expect(screen.getByText('A chart shows at most 6 series.')).toBeInTheDocument()
  })

  it('says so when there is no data', () => {
    render(<TimeSeriesChart title="Empty" data={[]} series={series} />)
    expect(screen.getByText('No data in this range')).toBeInTheDocument()
  })

  it('has no accessibility violations in either view', async () => {
    const user = userEvent.setup()
    const { container } = render(<TimeSeriesChart title="Storage" data={data} series={series} />)
    await expectNoA11yViolations(container)
    await user.click(screen.getByRole('button', { name: 'Show data table' }))
    await expectNoA11yViolations(container)
  })
})

describe('createLabelPlacer', () => {
  it('leaves labels that are far apart where they are', () => {
    const place = createLabelPlacer(14)
    expect(place(20)).toBe(20)
    expect(place(100)).toBe(100)
  })

  it('pushes a label that would overlap an earlier one below it', () => {
    const place = createLabelPlacer(14)
    expect(place(100)).toBe(100)
    expect(place(105)).toBe(114)
    // A third one at the same height keeps stacking, never overlapping.
    expect(place(100)).toBe(128)
  })

  it('must_not_loop_forever_on_a_crowd', () => {
    const place = createLabelPlacer(14)
    const ys = Array.from({ length: 30 }, () => place(50))
    expect(new Set(ys).size).toBeGreaterThan(15)
  })
})

describe('colors', () => {
  it('must_not_put_var_references_into_svg_attributes (they fall back to black)', () => {
    const { container } = render(<TimeSeriesChart title="Storage" data={data} series={series} />)
    for (const el of container.querySelectorAll('[stroke], [fill]')) {
      expect(el.getAttribute('stroke') ?? '').not.toContain('var(')
      expect(el.getAttribute('fill') ?? '').not.toContain('var(')
    }
  })
})
