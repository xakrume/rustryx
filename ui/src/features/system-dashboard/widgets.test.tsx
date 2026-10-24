import { QueryClientProvider } from '@tanstack/react-query'
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from '@tanstack/react-router'
import { configure, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import type { ReactNode } from 'react'
import { beforeEach, describe, expect, it } from 'vitest'
import { Toaster } from '@/components/toaster'
import { TooltipProvider } from '@/components/ui/tooltip'
import { ThemeProvider } from '@/features/theme/theme-provider'
import { handlers } from '@/mocks/handlers'
import { startSession } from '@/mocks/session'
import { createTestQueryClient, expectNoA11yViolations } from '@/test/utils'
import { server } from '@/test/server'
import { JobHealthWidget } from './jobs-widget'
import { RequestSummaryWidget } from './request-widget'
import { StorageTrendWidget } from './storage-widget'
import { SystemDashboardWidgets } from './system-widgets'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

/** The widgets are placed by the dashboard; a bare router gives their links something to resolve. */
function renderWidget(ui: ReactNode, user: 'admin' | 'alice' = 'admin') {
  server.use(...handlers)
  startSession(user)
  const root = createRootRoute({ component: () => <>{ui}</> })
  const router = createRouter({
    routeTree: root,
    history: createMemoryHistory({ initialEntries: ['/'] }),
  })
  return render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ThemeProvider>
        <TooltipProvider delayDuration={0}>
          <RouterProvider router={router} />
          <Toaster />
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>,
  )
}

const envelope = (code: string, status: number) =>
  HttpResponse.json(
    { error: { code, message: 'Failed', request_id: `req-${code}` } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

const store = (name: string, used: number, extra: Record<string, unknown> = {}) => ({
  id: `id-${name}`,
  name,
  kind: 'local',
  config: { path: `/data/${name}` },
  quota_bytes: null,
  created_at: '2026-10-01T00:00:00Z',
  usage: {
    used_bytes: used,
    blob_count: 1,
    available_bytes: 100,
    total_bytes: used + 100,
    as_of: '2026-10-24T09:00:00Z',
    ...extra,
  },
  health: { state: 'ok', checked_at: null, last_error_code: null },
  repository_count: 1,
  credentials: null,
})

beforeEach(() => window.sessionStorage.clear())

describe('storage trend widget', () => {
  it('draws the trend when the server reports history, with the data table one click away', async () => {
    const user = userEvent.setup()
    const { container } = renderWidget(<StorageTrendWidget />)
    const figure = await screen.findByRole('figure')
    expect(within(figure).getByText('Storage used by blob store')).toBeInTheDocument()
    const toggle = within(figure).getByRole('button', { name: 'Show data table' })
    await user.click(toggle)
    const table = within(figure).getAllByRole('table')[0]!
    expect(within(table).getByRole('columnheader', { name: 'default' })).toBeInTheDocument()
    expect(within(table).getByRole('columnheader', { name: 'fast-ssd' })).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('must_always_offer_a_data_table_current_usage_when_there_is_no_history', async () => {
    renderWidget(<StorageTrendWidget />, 'admin')
    // Always a real table of the current state, with quota and health in words.
    const table = await screen.findByRole('region', { name: 'Current usage of blob stores' })
    expect(within(table).getByRole('link', { name: 'default' })).toBeInTheDocument()
    expect(within(table).getAllByRole('meter').length).toBeGreaterThan(2)
    expect(within(table).getAllByText('Healthy').length).toBeGreaterThan(0)
  })

  it('must_not_invent_a_trend_when_the_server_sends_no_history', async () => {
    server.use(...handlers)
    renderWidgetWith([store('only', 5_000_000_000), store('other', 1_000_000_000)])
    expect(await screen.findByText(/no trend to draw/)).toBeInTheDocument()
    expect(screen.queryByRole('figure')).not.toBeInTheDocument()
    expect(
      await screen.findByRole('region', { name: 'Current usage of blob stores' }),
    ).toBeInTheDocument()
  })

  it('shows only the largest stores and says how many there are', async () => {
    const many = Array.from({ length: 8 }, (_, i) => store(`store-${i}`, (i + 1) * 1_000_000_000))
    renderWidgetWith(many)
    expect(await screen.findByText('Showing the 6 largest stores of 8.')).toBeInTheDocument()
    const table = screen.getByRole('region', { name: 'Current usage of blob stores' })
    expect(within(table).getByRole('link', { name: 'store-7' })).toBeInTheDocument()
    expect(within(table).queryByRole('link', { name: 'store-0' })).not.toBeInTheDocument()
  })

  it('shows an empty state without blob stores', async () => {
    renderWidgetWith([])
    expect(await screen.findByText('No blob stores yet.')).toBeInTheDocument()
  })

  it('shows its own error with code and request id and retries', async () => {
    let calls = 0
    server.use(...handlers)
    server.use(
      http.get('/api/v1/blob-stores', () => {
        calls += 1
        return envelope('store_unavailable', 503)
      }),
    )
    startSession('admin')
    renderWidgetRaw(<StorageTrendWidget />)
    expect(await screen.findByText('This widget could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('req-store_unavailable')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(calls).toBeGreaterThan(1))
  })

  it('says the widget needs the admin role when the server answers 403', async () => {
    server.use(...handlers)
    server.use(http.get('/api/v1/blob-stores', () => envelope('forbidden', 403)))
    startSession('admin')
    renderWidgetRaw(<StorageTrendWidget />)
    expect(await screen.findByText('Requires the nx-admin role.')).toBeInTheDocument()
    expect(
      screen.queryByRole('heading', { name: 'You do not have access' }),
    ).not.toBeInTheDocument()
  })
})

function renderWidgetWith(items: unknown[]) {
  server.use(...handlers)
  server.use(http.get('/api/v1/blob-stores', () => HttpResponse.json({ items, next: null })))
  startSession('admin')
  return renderWidgetRaw(<StorageTrendWidget />)
}

function renderWidgetRaw(ui: ReactNode) {
  const root = createRootRoute({ component: () => <>{ui}</> })
  const router = createRouter({
    routeTree: root,
    history: createMemoryHistory({ initialEntries: ['/'] }),
  })
  return render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ThemeProvider>
        <TooltipProvider delayDuration={0}>
          <RouterProvider router={router} />
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>,
  )
}

describe('request summary widget', () => {
  it('shows counters, the error rate in words and the scope of the numbers', async () => {
    const { container } = renderWidget(<RequestSummaryWidget />)
    expect(await screen.findByText('Requests')).toBeInTheDocument()
    expect(screen.getByText('Server errors (5xx)')).toBeInTheDocument()
    expect(screen.getByText('87 ms')).toBeInTheDocument()
    expect(screen.getByText('Normal')).toBeInTheDocument()
    expect(screen.getByText(/Node rustryx-0, up 4 d 1 h/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('warns in words when more than one percent of requests fail', async () => {
    server.use(...handlers)
    server.use(
      http.get('/api/v1/system/metrics/summary', () =>
        HttpResponse.json({
          node: 'rustryx-0',
          generated_at: '2026-10-24T09:00:00Z',
          uptime_secs: 100,
          http: {
            requests_total: 1000,
            errors_5xx_total: 50,
            in_flight: 1,
            shed_total: 0,
            p95_ms: null,
          },
          auth: { logins_ok_total: 1, logins_failed_total: 0, lockouts_total: 0 },
          database: { pool_size: 1, pool_in_use: 0, pool_wait_p95_ms: null },
          background_jobs: [],
        }),
      ),
    )
    startSession('admin')
    renderWidgetRaw(<RequestSummaryWidget />)
    expect(await screen.findByText('Above 1 percent')).toBeInTheDocument()
    expect(screen.getByText('5.00%')).toBeInTheDocument()
    expect(screen.getByText('n/a')).toBeInTheDocument()
  })
})

describe('job health widget', () => {
  it('lists the jobs with their last success and the tasks that failed', async () => {
    window.sessionStorage.setItem('ryx-mock-status', 'mixed')
    const { container } = renderWidget(<JobHealthWidget />)
    const list = await screen.findByRole('list', { name: 'Background jobs' })
    expect(within(list).getByText('blob-gc')).toBeInTheDocument()
    expect(within(list).getByText('Stale')).toBeInTheDocument()
    expect(within(list).getByText(/Never/)).toBeInTheDocument()
    // Failed and aborted tasks link to their run.
    expect(await screen.findByRole('link', { name: 'Cleanup policies' })).toHaveAttribute(
      'href',
      expect.stringContaining('/admin/tasks/'),
    )
    expect(screen.getByText('blob_store_unavailable')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('keeps working when only the task list fails', async () => {
    server.use(...handlers)
    server.use(http.get('/api/v1/tasks', () => envelope('internal_error', 500)))
    startSession('admin')
    renderWidgetRaw(<JobHealthWidget />)
    expect(await screen.findByText('This widget could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('req-internal_error')).toBeInTheDocument()
  })
})

describe('system widgets block', () => {
  it('shows the three widgets to an administrator', async () => {
    renderWidget(<SystemDashboardWidgets />)
    expect(await screen.findByText('Storage usage trend')).toBeInTheDocument()
    expect(screen.getByText('Requests and errors')).toBeInTheDocument()
    expect(screen.getByText('Job health')).toBeInTheDocument()
  })

  it('must_not_show_the_block_to_a_non_administrator', async () => {
    renderWidget(<SystemDashboardWidgets />, 'alice')
    // The principal arrives first; then the gate decides. Nothing of the block is ever shown.
    await new Promise((resolve) => setTimeout(resolve, 100))
    expect(screen.queryByText('Storage usage trend')).not.toBeInTheDocument()
    expect(screen.queryByText('Job health')).not.toBeInTheDocument()
  })
})
