import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { Inbox } from 'lucide-react'
import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import { Button } from '@/components/ui/button'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { EmptyState } from './empty-state'
import { ErrorPanel } from './error-panel'
import { ForbiddenPanel } from './forbidden-panel'
import { PageHeader } from './page-header'

const apiError = (
  status: number,
  code: string,
  extra: Partial<ConstructorParameters<typeof ApiError>[0]> = {},
) => new ApiError({ code, message: `message of ${code}`, status, requestId: 'req-77', ...extra })

describe('ErrorPanel', () => {
  it('shows the server message, the stable code and the request id with copy buttons', async () => {
    const user = userEvent.setup()
    const retry = vi.fn()
    const { container } = renderWithProviders(
      <ErrorPanel error={apiError(500, 'internal_error')} onRetry={retry} />,
    )
    expect(screen.getByRole('alert')).toHaveTextContent('message of internal_error')
    expect(screen.getByText('internal_error')).toBeInTheDocument()
    expect(screen.getByText('req-77')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Copy Request ID' }))
    expect(await navigator.clipboard.readText()).toBe('req-77')
    await user.click(screen.getByRole('button', { name: 'Retry' }))
    expect(retry).toHaveBeenCalledTimes(1)
    await expectNoA11yViolations(container)
  })

  it('explains a 404 as not found or hidden', () => {
    renderWithProviders(<ErrorPanel error={apiError(404, 'repository_not_found')} />)
    expect(screen.getByText('Not found or hidden from you')).toBeInTheDocument()
    expect(screen.getByText(/may not exist, or you may not be allowed/)).toBeInTheDocument()
  })

  it('keeps Retry disabled with a countdown while the server asks to wait', () => {
    renderWithProviders(
      <ErrorPanel
        error={apiError(429, 'rate_limited', { retryAfterSecs: 20 })}
        onRetry={vi.fn()}
      />,
    )
    const retry = screen.getByRole('button', { name: /Retry in \d+ s/ })
    expect(retry).toBeDisabled()
  })

  it('shows the forbidden panel instead of a retry for a 403', () => {
    renderWithProviders(<ErrorPanel error={apiError(403, 'forbidden')} onRetry={vi.fn()} />)
    expect(screen.getByRole('heading', { name: 'You do not have access' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument()
  })

  it('treats an unexpected thrown value generically and never shows its text', () => {
    renderWithProviders(<ErrorPanel error={new Error('secret internal detail')} />)
    expect(screen.queryByText(/secret internal detail/)).not.toBeInTheDocument()
    expect(screen.getByText('Something went wrong')).toBeInTheDocument()
  })

  it('says the server is unreachable for a network failure', () => {
    renderWithProviders(
      <ErrorPanel error={new ApiError({ code: 'network_error', message: 'x', status: 0 })} />,
    )
    expect(screen.getByText('The server is not reachable')).toBeInTheDocument()
  })
})

describe('ForbiddenPanel', () => {
  it('names the missing privilege and who can grant it', async () => {
    const { container } = renderWithProviders(<ForbiddenPanel privilege="nx-admin" />)
    expect(screen.getByText('Missing privilege: nx-admin')).toBeInTheDocument()
    expect(screen.getByText(/administrator can grant it/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })
})

describe('EmptyState and PageHeader', () => {
  it('shows text, one action and no illustration', async () => {
    const { container } = renderWithProviders(
      <EmptyState
        icon={Inbox}
        title="No repositories yet"
        description="Create one."
        action={<Button>Create repository</Button>}
      />,
    )
    expect(screen.getByRole('heading', { name: 'No repositories yet' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Create repository' })).toBeInTheDocument()
    expect(container.querySelector('img')).toBeNull()
    await expectNoA11yViolations(container)
  })

  it('renders one focusable h1 and sets the document title', () => {
    renderWithProviders(
      <PageHeader title="Repositories" description="All of them" actions={<Button>New</Button>} />,
    )
    const h1 = screen.getByRole('heading', { level: 1, name: 'Repositories' })
    expect(h1).toHaveAttribute('tabindex', '-1')
    expect(h1).toHaveAttribute('data-page-title')
    expect(document.title).toBe('Repositories - Rustryx')
  })
})
