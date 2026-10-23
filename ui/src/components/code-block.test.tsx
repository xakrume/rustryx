import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { CodeBlock } from './code-block'

describe('CodeBlock', () => {
  it('shows a command in a labelled, focusable code region with a copy button', async () => {
    const user = userEvent.setup()
    const { container } = renderWithProviders(
      <CodeBlock label="Pull command" value="docker pull repo/app:1.0" />,
    )
    expect(screen.getByText('docker pull repo/app:1.0').closest('pre')).toHaveAccessibleName(
      'Pull command',
    )
    expect(screen.getByText('docker pull repo/app:1.0').closest('pre')).toHaveAttribute(
      'tabindex',
      '0',
    )
    await user.click(screen.getByRole('button', { name: 'Copy Pull command' }))
    expect(await navigator.clipboard.readText()).toBe('docker pull repo/app:1.0')
    await expectNoA11yViolations(container)
  })

  it('must_not_put_a_hidden_secret_into_the_dom', () => {
    const { container } = renderWithProviders(
      <CodeBlock label="API token" value="ryx_TOPSECRET123" secret />,
    )
    // Select all, find in page and screen reader browsing read the DOM: the value is not there.
    expect(container.textContent).not.toContain('TOPSECRET')
    expect(container.innerHTML).not.toContain('TOPSECRET')
    expect(screen.getByLabelText(/Hidden/)).toBeInTheDocument()
  })

  it('still copies the real secret while it is hidden, and reveals it on request', async () => {
    const user = userEvent.setup()
    renderWithProviders(<CodeBlock label="API token" value="ryx_TOPSECRET123" secret />)
    await user.click(screen.getByRole('button', { name: 'Copy API token' }))
    expect(await navigator.clipboard.readText()).toBe('ryx_TOPSECRET123')

    const reveal = screen.getByRole('button', { name: 'Show value' })
    expect(reveal).toHaveAttribute('aria-pressed', 'false')
    await user.click(reveal)
    expect(screen.getByText('ryx_TOPSECRET123')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Hide value' })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
  })
})
