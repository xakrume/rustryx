import { screen, within } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { renderApp } from '@/test/utils'

describe('dashboard', () => {
  it('shows an anonymous visitor the public repositories only, and says so', async () => {
    renderApp('/')
    expect(await screen.findByText(/You see public repositories only/)).toBeInTheDocument()
    const main = within(screen.getByRole('main'))
    expect(await main.findByRole('heading', { name: 'Repositories' })).toBeInTheDocument()
    expect(main.queryByRole('heading', { name: 'Storage' })).not.toBeInTheDocument()
    expect(main.queryByRole('heading', { name: 'Recent activity' })).not.toBeInTheDocument()
  })

  it('shows an administrator storage, setup steps and recent activity', async () => {
    renderApp('/', { user: 'admin' })
    // The navigation has headings of its own ("Storage"), so look inside the page.
    const main = within(await screen.findByRole('main'))
    expect(await main.findByRole('heading', { name: 'Storage' })).toBeInTheDocument()
    expect(main.getByText('Degraded')).toBeInTheDocument()
    expect(main.getByRole('link', { name: 'Create an access token' })).toHaveAttribute(
      'href',
      '/me/tokens',
    )
    expect(main.getByRole('heading', { name: 'Recent activity' })).toBeInTheDocument()
    expect(main.getByText('role.update')).toBeInTheDocument()
  })

  it('shows the error with its code and request id and a retry when the overview fails', async () => {
    renderApp('/', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/dashboard/summary', () =>
          HttpResponse.json(
            {
              error: {
                code: 'store_unavailable',
                message: 'The database is not reachable',
                request_id: 'req-db',
              },
            },
            { status: 503, headers: { 'X-Request-Id': 'req-db' } },
          ),
        ),
      ],
    })
    expect(await screen.findByText('The overview could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-db')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Retry/ })).toBeInTheDocument()
  })
})
