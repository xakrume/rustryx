import { act, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import { toast } from '@/lib/toast-store'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'

describe('Toaster', () => {
  it('shows a toast in a labelled notification region and dismisses it', async () => {
    const user = userEvent.setup()
    renderWithProviders(<p>page</p>)
    act(() => void toast.success('Repository created', { description: 'maven-releases' }))
    expect(await screen.findByText('Repository created')).toBeInTheDocument()
    expect(screen.getByText('maven-releases')).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'Notifications' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Dismiss notification' }))
    await vi.waitFor(() => expect(screen.queryByText('Repository created')).not.toBeInTheDocument())
  })

  it('shows the stable code and request id of a failure, each with a copy button', async () => {
    const user = userEvent.setup()
    renderWithProviders(<p>page</p>)
    act(() => {
      toast.apiError(
        new ApiError({
          code: 'quota_exceeded',
          message: 'Quota exceeded',
          status: 507,
          requestId: 'req-123',
        }),
        'Upload failed',
      )
    })
    expect(await screen.findByText('Upload failed')).toBeInTheDocument()
    expect(screen.getByText('quota_exceeded')).toBeInTheDocument()
    expect(screen.getByText('req-123')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Copy Request ID' }))
    expect(await navigator.clipboard.readText()).toBe('req-123')
  })

  it('runs a toast action', async () => {
    const user = userEvent.setup()
    const undo = vi.fn()
    renderWithProviders(<p>page</p>)
    act(() => void toast.info('Token revoked', { action: { label: 'Undo', onClick: undo } }))
    await user.click(await screen.findByRole('button', { name: 'Undo' }))
    expect(undo).toHaveBeenCalled()
  })

  it('has no accessibility violations', async () => {
    renderWithProviders(<p>page</p>)
    act(() => void toast.warning('Quota almost reached', { description: 'default blob store' }))
    await screen.findByText('Quota almost reached')
    await expectNoA11yViolations(document.body)
  })
})
