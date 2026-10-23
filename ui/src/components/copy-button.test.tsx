import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { CopyButton } from './copy-button'

describe('CopyButton', () => {
  it('copies the value, confirms with a toast and a changed icon, and is accessible', async () => {
    const user = userEvent.setup()
    const { container } = renderWithProviders(<CopyButton value="sha256:abc123" what="digest" />)
    const button = screen.getByRole('button', { name: 'Copy digest' })
    await user.click(button)
    expect(await navigator.clipboard.readText()).toBe('sha256:abc123')
    expect(await screen.findByText('Copied')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('never renders the copied value itself', () => {
    const { container } = renderWithProviders(<CopyButton value="ryx_secret_value" what="token" />)
    expect(container.textContent).not.toContain('ryx_secret_value')
  })

  it('shows a visible label when asked', () => {
    renderWithProviders(<CopyButton value="x" what="URL" showLabel />)
    expect(screen.getByRole('button', { name: 'Copy URL' })).toHaveTextContent('Copy URL')
  })

  it('reports a failed copy instead of failing silently', async () => {
    const user = userEvent.setup()
    renderWithProviders(<CopyButton value="x" what="digest" />)
    vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('denied'))
    // jsdom has no execCommand either, so both paths fail.
    await user.click(screen.getByRole('button', { name: 'Copy digest' }))
    await waitFor(() => expect(screen.getByText(/Copy failed/)).toBeInTheDocument())
  })
})
