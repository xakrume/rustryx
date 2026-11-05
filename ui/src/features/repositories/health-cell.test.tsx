import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import type { RepositoryView } from './api'
import { HealthCell } from './health-cell'

type Health = NonNullable<RepositoryView['health']>

function health(overrides: Partial<Health> = {}): Health {
  return {
    scanned: true,
    component_count: 6,
    scanned_components: 6,
    critical: 0,
    severe: 0,
    moderate: 0,
    licenses: { copyleft: 0, non_standard: 0, other: 0 },
    scanned_at: '2026-11-05T08:00:00Z',
    ...overrides,
  }
}

describe('HealthCell', () => {
  it('says plainly that a repository was not scanned', () => {
    renderWithProviders(<HealthCell health={null} />)
    expect(screen.getByText('Not scanned')).toBeInTheDocument()
  })

  it('says "No issues" when a scan found nothing', () => {
    renderWithProviders(<HealthCell health={health()} />)
    expect(screen.getByRole('button', { name: /No known vulnerabilities/ })).toHaveTextContent(
      'No issues',
    )
  })

  it('names the worst severity in words, with the rest as a count', async () => {
    const { container } = renderWithProviders(
      <HealthCell health={health({ critical: 2, severe: 63, moderate: 102 })} />,
    )
    const button = screen.getByRole('button', {
      name: '2 critical, 63 severe, 102 moderate vulnerabilities. 6 of 6 components scanned.',
    })
    expect(button).toHaveTextContent('2 critical')
    expect(button).toHaveTextContent('+165 more')
    await expectNoA11yViolations(container)
  })

  it('shows the full breakdown and the scan coverage on click', async () => {
    const user = userEvent.setup()
    renderWithProviders(<HealthCell health={health({ critical: 2, severe: 63, moderate: 102 })} />)
    await user.click(screen.getByRole('button'))
    const dialog = await screen.findByRole('dialog', { name: 'Vulnerabilities' })
    expect(dialog).toHaveTextContent('Critical2')
    expect(dialog).toHaveTextContent('Severe63')
    expect(dialog).toHaveTextContent('Moderate102')
    expect(dialog).toHaveTextContent('6 of 6 components scanned')
  })

  it('leads with severe when nothing is critical', () => {
    renderWithProviders(<HealthCell health={health({ severe: 4, moderate: 1 })} />)
    expect(screen.getByRole('button')).toHaveTextContent('4 severe')
  })

  it('must_not_hide_that_a_scan_is_incomplete', () => {
    renderWithProviders(<HealthCell health={health({ scanned_components: 4 })} />)
    expect(screen.getByRole('button', { name: /4 of 6 components scanned/ })).toHaveTextContent(
      'Partial',
    )
  })
})
