import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { expectNoA11yViolations } from '@/test/utils'
import { StatusBadge, type StatusKind } from './status-badge'

const KINDS: StatusKind[] = ['ok', 'warning', 'error', 'info', 'neutral', 'unknown']

describe('StatusBadge', () => {
  it('must_not_rely_on_color_alone: every status has text and an icon', () => {
    const { container } = render(
      <ul>
        {KINDS.map((kind) => (
          <li key={kind}>
            <StatusBadge status={kind} />
          </li>
        ))}
      </ul>,
    )
    for (const li of container.querySelectorAll('li')) {
      expect(li.textContent?.trim().length).toBeGreaterThan(0)
      expect(li.querySelector('svg[aria-hidden="true"]')).not.toBeNull()
    }
  })

  it('shows a custom label next to the icon', () => {
    render(<StatusBadge status="error" label="Unreachable" />)
    expect(screen.getByText('Unreachable')).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const { container } = render(<StatusBadge status="warning" label="Degraded" />)
    await expectNoA11yViolations(container)
  })
})
