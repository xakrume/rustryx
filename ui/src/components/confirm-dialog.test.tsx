import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { ConfirmDialog } from './confirm-dialog'

function setup(props: Partial<React.ComponentProps<typeof ConfirmDialog>> = {}) {
  const onOpenChange = vi.fn()
  const onConfirm = vi.fn(() => Promise.resolve())
  renderWithProviders(
    <ConfirmDialog
      open
      onOpenChange={onOpenChange}
      kind="repository"
      name="maven-releases"
      onConfirm={onConfirm}
      {...props}
    />,
  )
  return { onOpenChange, onConfirm }
}

describe('ConfirmDialog', () => {
  it('names the object in the title and the description', async () => {
    setup()
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete repository\s+maven-releases\s*\?/,
    })
    expect(dialog).toHaveAccessibleDescription(/permanently delete repository maven-releases/)
    await expectNoA11yViolations(document.body)
  })

  it('puts the initial focus on Cancel, the safe choice', async () => {
    setup()
    await screen.findByRole('alertdialog')
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus()
  })

  it('confirms and closes', async () => {
    const user = userEvent.setup()
    const { onConfirm, onOpenChange } = setup()
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
    expect(onConfirm).toHaveBeenCalledTimes(1)
  })

  it('must_not_enable_confirm_until_the_exact_name_is_typed', async () => {
    const user = userEvent.setup()
    const { onConfirm } = setup({ requireTyping: true, confirmLabel: 'Delete repository' })
    const confirm = await screen.findByRole('button', { name: 'Delete repository' })
    const input = screen.getByRole('textbox', { name: 'Confirmation text' })
    // Typing is required, so the input (not Cancel) is where focus starts.
    expect(input).toHaveFocus()
    expect(confirm).toBeDisabled()
    await user.type(input, 'maven-release')
    expect(confirm).toBeDisabled()
    await user.type(input, 'S')
    expect(confirm).toBeDisabled()
    await user.clear(input)
    await user.type(input, 'maven-releases')
    expect(confirm).toBeEnabled()
    await user.click(confirm)
    expect(onConfirm).toHaveBeenCalledTimes(1)
  })

  it('keeps the dialog open and shows the error code and request id when the action fails', async () => {
    const user = userEvent.setup()
    const failure = new ApiError({
      code: 'internal_error',
      message: 'The server failed to handle the request',
      status: 500,
      requestId: 'req-42',
    })
    const { onOpenChange } = setup({ onConfirm: () => Promise.reject(failure) })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    expect(await screen.findByText('internal_error')).toBeInTheDocument()
    expect(screen.getByText('req-42')).toBeInTheDocument()
    expect(onOpenChange).not.toHaveBeenCalledWith(false)
    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
  })

  it('lists the objects that block the change', async () => {
    const user = userEvent.setup()
    setup({
      dependents: [{ kind: 'repository', id: '1', name: 'npm-group' }],
      onConfirm: () =>
        Promise.reject(
          new ApiError({
            code: 'resource_in_use',
            message: 'in use',
            status: 409,
            details: { dependents: [{ kind: 'repository', id: '2', name: 'pypi-group' }] },
          }),
        ),
    })
    expect(await screen.findByText('npm-group')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Delete' }))
    // The 409 body is shown as well, with the same list.
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
  })
})
