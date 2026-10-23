import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { SecretOnce } from './secret-once'

const SECRET = 'ryx_Qk3Tm9Zp0aLw7Vb2XyHn5CdEf8GjRs1UoIq4KtAx6Ml'

describe('SecretOnce', () => {
  it('shows the secret once with a copy button and an acknowledgement', async () => {
    renderWithProviders(<SecretOnce secret={SECRET} what="API token" onDone={vi.fn()} />)
    const dialog = await screen.findByRole('dialog', { name: 'Copy your API token now' })
    expect(dialog).toHaveTextContent(SECRET)
    expect(screen.getByRole('button', { name: 'Copy API token' })).toBeInTheDocument()
    await expectNoA11yViolations(document.body)
  })

  it('must_not_close_on_escape_or_outside_click_before_acknowledgement', async () => {
    const user = userEvent.setup()
    const onDone = vi.fn()
    renderWithProviders(<SecretOnce secret={SECRET} what="API token" onDone={onDone} />)
    await screen.findByRole('dialog')
    await user.keyboard('{Escape}')
    await user.click(
      document.querySelector('[aria-hidden="true"][data-state="open"]') as HTMLElement,
    )
    expect(screen.getByRole('dialog')).toBeInTheDocument()
    expect(onDone).not.toHaveBeenCalled()
    expect(screen.getByRole('button', { name: 'Done' })).toBeDisabled()
    expect(screen.queryByRole('button', { name: 'Close' })).not.toBeInTheDocument()
  })

  it('enables Done after the acknowledgement and then lets Escape close it', async () => {
    const user = userEvent.setup()
    const onDone = vi.fn()
    renderWithProviders(<SecretOnce secret={SECRET} what="API token" onDone={onDone} />)
    await user.click(await screen.findByRole('checkbox', { name: 'I stored it in a safe place' }))
    const done = screen.getByRole('button', { name: 'Done' })
    expect(done).toBeEnabled()
    await user.click(done)
    expect(onDone).toHaveBeenCalledTimes(1)
    await user.keyboard('{Escape}')
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(2))
  })

  it('renders nothing while there is no secret', () => {
    renderWithProviders(<SecretOnce secret={null} what="API token" onDone={vi.fn()} />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })
})
