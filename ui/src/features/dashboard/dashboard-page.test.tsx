import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { renderApp } from '@/test/utils'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

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
    expect(main.getByRole('heading', { name: 'Engineer tasks' })).toBeInTheDocument()
    expect(main.getByRole('link', { name: 'Routing rules' })).toHaveAttribute(
      'href',
      '/admin/routing-rules',
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

  it('offers the actions the caller may take and links the counts to the filtered list', async () => {
    const { router } = renderApp('/', { user: 'admin' })
    const main = within(await screen.findByRole('main'))
    expect(await main.findByRole('link', { name: 'Create repository' })).toHaveAttribute(
      'href',
      '/repositories/new',
    )
    expect(main.getByRole('link', { name: 'Upload' })).toBeInTheDocument()
    expect(main.getByRole('link', { name: 'Create a token' })).toHaveAttribute('href', '/me/tokens')
    expect(await main.findByRole('link', { name: 'View audit log' })).toHaveAttribute(
      'href',
      '/admin/audit',
    )
    await userEvent.setup().click(await main.findByRole('link', { name: /^Maven/ }))
    await waitFor(() => expect(router.state.location.search).toMatchObject({ format: 'maven2' }))
  })

  it('must_not_offer_creating_repositories_to_a_developer', async () => {
    renderApp('/', { user: 'alice' })
    const main = within(await screen.findByRole('main'))
    expect(await main.findByRole('link', { name: 'Upload' })).toBeInTheDocument()
    expect(main.queryByRole('link', { name: 'Create repository' })).not.toBeInTheDocument()
    expect(
      await main.findByRole('link', { name: 'Search packages and images' }),
    ).toBeInTheDocument()
    expect(main.queryByRole('link', { name: 'Routing rules' })).not.toBeInTheDocument()
    expect(main.queryByRole('link', { name: 'View audit log' })).not.toBeInTheDocument()
  })

  it('invites an anonymous visitor to sign in and shows no actions', async () => {
    renderApp('/')
    const main = within(await screen.findByRole('main'))
    expect(await main.findByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login')
    expect(main.queryByRole('link', { name: 'Upload' })).not.toBeInTheDocument()
  })

  it('shows a degraded system, failed tasks and a store that is nearly full', async () => {
    renderApp('/', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/dashboard/summary', () =>
          HttpResponse.json({
            public_only: false,
            repositories: { total: 2, offline: 0, by_format: { npm: 2 }, by_type: { hosted: 2 } },
            storage: [{ blob_store: 'fast', used_bytes: 95, quota_bytes: 100, health: 'ok' }],
            recent_activity: [],
            failed_tasks: [
              {
                id: '0192f6c4-5a1e-7000-8000-00000000008d',
                task_id: '0192f6c4-5a1e-7000-8000-000000000097',
                status: 'failed',
                dry_run: false,
                trigger: 'schedule',
                triggered_by: null,
                node: 'rustryx-0',
                queued_at: '2026-10-24T09:00:00Z',
                started_at: null,
                finished_at: '2026-10-24T09:01:00Z',
                duration_ms: null,
                summary: {},
                error_code: 'blob_store_unavailable',
                log_available: false,
                log_expires_at: null,
              },
            ],
            setup_checklist: [],
            degraded: true,
          }),
        ),
      ],
    })
    const main = within(await screen.findByRole('main'))
    expect(await main.findByText('Some system checks are not healthy.')).toBeInTheDocument()
    expect(main.getByRole('link', { name: 'View status' })).toHaveAttribute(
      'href',
      '/admin/system/status',
    )
    expect(main.getByRole('heading', { name: 'Failed tasks' })).toBeInTheDocument()
    expect(main.getByText('blob_store_unavailable')).toBeInTheDocument()
    expect(main.getByRole('progressbar', { name: 'Usage of fast' })).toHaveAttribute(
      'aria-valuenow',
      '95',
    )
    expect(main.getByText(/\(95%\)/)).toBeInTheDocument()
  })
})
