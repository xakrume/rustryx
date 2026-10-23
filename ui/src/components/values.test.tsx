import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { TooltipProvider } from '@/components/ui/tooltip'
import { expectNoA11yViolations } from '@/test/utils'
import { BytesText } from './bytes-text'
import { RelativeTime } from './relative-time'

describe('BytesText', () => {
  it('shows binary units with the exact byte count as the tooltip', () => {
    render(<BytesText bytes={8_123_456_789} />)
    const el = screen.getByText(/GiB/)
    expect(el).toHaveAttribute('title', expect.stringContaining('8,123,456,789 bytes'))
    expect(el).toHaveClass('tabular-nums')
  })

  it('shows a dash and no tooltip for an unknown size', () => {
    render(<BytesText bytes={null} />)
    const el = screen.getByText('-')
    expect(el).not.toHaveAttribute('title')
  })
})

describe('RelativeTime', () => {
  it('shows relative text, a machine readable time and the absolute time for screen readers', () => {
    vi.useFakeTimers({ now: new Date('2026-10-24T12:00:00Z'), toFake: ['Date'] })
    const { container } = render(
      <TooltipProvider>
        <RelativeTime value="2026-10-24T11:50:00Z" />
      </TooltipProvider>,
    )
    const time = container.querySelector('time')
    expect(time).toHaveAttribute('datetime', '2026-10-24T11:50:00.000Z')
    expect(time).toHaveTextContent('10 minutes ago')
    // The absolute time is in the DOM (visually hidden), not only in a tooltip.
    expect(time?.querySelector('.sr-only')?.textContent).toMatch(/2026/)
    vi.useRealTimers()
  })

  it('shows the absolute time and the UTC instant in a tooltip on hover', async () => {
    const user = userEvent.setup()
    render(
      <TooltipProvider delayDuration={0}>
        <RelativeTime value="2026-10-24T11:50:00Z" />
      </TooltipProvider>,
    )
    await user.hover(document.querySelector('time') as HTMLElement)
    expect((await screen.findAllByText('2026-10-24T11:50:00.000Z')).length).toBeGreaterThan(0)
  })

  it('shows a dash for never', () => {
    render(<RelativeTime value={null} />)
    expect(screen.getByText('-')).toBeInTheDocument()
  })

  it('can be a tab stop, and has no accessibility violations', async () => {
    const { container } = render(
      <TooltipProvider>
        <RelativeTime value="2026-10-24T11:50:00Z" focusable />
      </TooltipProvider>,
    )
    expect(container.querySelector('time')).toHaveAttribute('tabindex', '0')
    await expectNoA11yViolations(container)
  })
})
