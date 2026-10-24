import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mockId } from '@/features/system/mock-kit'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetLifecycleMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const run = (n: number) => mockId(3, n)
const gc = (n: number) => mockId(5, n)

beforeEach(() => resetLifecycleMock())
afterEach(() => vi.restoreAllMocks())

describe('lifecycle run history', () => {
  it('lists cleanup runs with mode, status, counts and sizes, and opens a report', async () => {
    const user = userEvent.setup()
    const { container } = renderApp('/admin/tasks?view=lifecycle', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Lifecycle runs' })
    expect(await within(table).findAllByText('Dry run')).not.toHaveLength(0)
    expect(within(table).getAllByText('Real run').length).toBeGreaterThan(0)
    expect(within(table).getAllByText('Failed').length).toBeGreaterThan(0)
    expect(within(table).getByText('412')).toBeInTheDocument()
    await expectNoA11yViolations(container)
    await user.click(
      within(table).getAllByRole('button', { name: 'Open report of maven-old-snapshots' })[0]!,
    )
    expect(await screen.findByRole('heading', { name: /Dry run report/ })).toBeInTheDocument()
    expect(await screen.findByText('Items to remove')).toBeInTheDocument()
    expect(screen.getByText('412')).toBeInTheDocument()
  })

  it('filters runs by mode', async () => {
    const user = userEvent.setup()
    renderApp('/admin/tasks?view=lifecycle', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Lifecycle runs' })
    await within(table).findAllByText('Dry run')
    await user.selectOptions(screen.getByLabelText('Mode'), 'real')
    await waitFor(() =>
      expect(within(table).queryByText('Dry run', { selector: 'span' })).not.toBeInTheDocument(),
    )
    expect(within(table).getAllByText('Real run').length).toBeGreaterThan(0)
  })

  it('shows garbage collection runs and their report per blob store', async () => {
    const user = userEvent.setup()
    renderApp('/admin/tasks?view=lifecycle', { user: 'admin' })
    await user.selectOptions(await screen.findByLabelText('Run kind'), 'gc')
    const table = await screen.findByRole('region', { name: 'Lifecycle runs' })
    await within(table).findAllByText('Succeeded')
    await user.click(within(table).getAllByRole('button', { name: /^Open report of/ })[0]!)
    expect(
      await screen.findByRole('heading', { name: /Garbage collection report/ }),
    ).toBeInTheDocument()
    expect(await screen.findByText('Per blob store')).toBeInTheDocument()
    expect(screen.getByRole('rowheader', { name: 'fast-ssd' })).toBeInTheDocument()
    expect(screen.getByText('Estimate for a dry run, actual for a real run')).toBeInTheDocument()
  })

  it('shows a clear empty state when nothing ran yet', async () => {
    renderApp('/admin/tasks?view=lifecycle', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/lifecycle-runs', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('No runs yet')).toBeInTheDocument()
  })
})

describe('dry run report', () => {
  it('shows the summary first and then the paged items with the reason of each', async () => {
    const user = userEvent.setup()
    const { container } = renderApp(`/admin/tasks?view=lifecycle&run=${run(1)}`, { user: 'admin' })
    expect(await screen.findByText('Items to remove')).toBeInTheDocument()
    expect(screen.getByText('412')).toBeInTheDocument()
    expect(screen.getByText('Reclaimable size')).toBeInTheDocument()
    expect(screen.getByText('18.2 GiB')).toBeInTheDocument()
    expect(screen.getByText('Kept by protection')).toBeInTheDocument()
    const table = await screen.findByRole('region', { name: 'Items of the plan' })
    expect(await within(table).findAllByText('Age')).not.toHaveLength(0)
    expect(within(table).getAllByText(/com\.example\.payments:ledger/).length).toBeGreaterThan(5)
    await expectNoA11yViolations(container)
    // The plan is paged: more rows arrive with "Load more", never all at once.
    const before = within(table).getAllByRole('row').length
    await user.click(screen.getByRole('button', { name: 'Load more' }))
    await waitFor(() => expect(within(table).getAllByRole('row').length).toBeGreaterThan(before))
  })

  it('filters the plan by protection', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/tasks?view=lifecycle&run=${run(1)}`, { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Items of the plan' })
    await within(table).findAllByText(/com\.example\.payments/)
    await user.selectOptions(screen.getByLabelText('Protection'), 'yes')
    await waitFor(() =>
      expect(within(table).queryByText('None', { selector: 'span' })).not.toBeInTheDocument(),
    )
    expect(within(table).getAllByText('immutability rule release-lock').length).toBeGreaterThan(0)
  })

  it('says that a failed run produced no report and names the error code', async () => {
    renderApp(`/admin/tasks?view=lifecycle&run=${run(3)}`, { user: 'admin' })
    expect(await screen.findByText('The run failed')).toBeInTheDocument()
    expect(
      screen.getByText('No report was produced. Error code: blob_store_unavailable'),
    ).toBeInTheDocument()
    expect(screen.queryByText('Items to remove')).not.toBeInTheDocument()
  })

  it('says when a report expired and offers a new dry run instead of stale numbers', async () => {
    renderApp(`/admin/tasks?view=lifecycle&run=${run(5)}`, { user: 'admin' })
    expect(await screen.findByText('This report is no longer available')).toBeInTheDocument()
    expect(screen.getByText(/Reports are kept for a limited time/)).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Items of the plan' })).not.toBeInTheDocument()
  })

  it('shows progress while the run is planning and the report when it is done', async () => {
    renderApp(`/admin/tasks?view=lifecycle&run=${run(4)}`, { user: 'admin' })
    expect(await screen.findByText('Planning what would be removed')).toBeInTheDocument()
    expect(await screen.findByText('Items to remove', {}, { timeout: 5000 })).toBeInTheDocument()
    expect(screen.queryByText('Planning what would be removed')).not.toBeInTheDocument()
  })

  it('shows an empty plan clearly', async () => {
    renderApp(`/admin/tasks?view=lifecycle&run=${run(7)}`, { user: 'admin' })
    expect(
      await screen.findByText('Nothing matches: no component would be removed.'),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Export CSV' })).not.toBeInTheDocument()
  })

  it('must_export_the_complete_plan_as_csv_not_only_the_loaded_page', async () => {
    const user = userEvent.setup()
    let blob: Blob | null = null
    // jsdom has no object URLs: install both for this test only.
    Object.defineProperty(URL, 'createObjectURL', {
      configurable: true,
      value: (value: Blob) => {
        blob = value
        return 'blob:mock'
      },
    })
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: () => undefined })
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined)
    renderApp(`/admin/tasks?view=lifecycle&run=${run(1)}`, { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Export CSV' }))
    await screen.findByText('The plan was exported')
    expect(blob).not.toBeNull()
    const text = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => resolve(typeof reader.result === 'string' ? reader.result : '')
      reader.onerror = () => reject(new Error('unreadable'))
      reader.readAsText(blob as unknown as Blob)
    })
    const lines = text.trim().split('\r\n')
    expect(lines[0]).toBe(
      'repository,coordinate,size_bytes,age_days,last_downloaded_at,reason,protected_by',
    )
    // Header plus every one of the 412 items: the table only ever held one page.
    expect(lines).toHaveLength(413)
  })

  it('shows the error with code and request id when the run cannot be loaded', async () => {
    renderApp(`/admin/tasks?view=lifecycle&run=${mockId(3, 9999)}`, { user: 'admin' })
    expect(await screen.findByText('lifecycle_run_not_found')).toBeInTheDocument()
  })
})

describe('gc report', () => {
  it('shows a failed garbage collection run without numbers it does not have', async () => {
    renderApp(`/admin/tasks?view=lifecycle&source=gc&run=${gc(3)}`, { user: 'admin' })
    expect(
      await screen.findByRole('heading', { name: /Garbage collection report/ }),
    ).toBeInTheDocument()
    expect(screen.getByText('Failed')).toBeInTheDocument()
    expect(screen.queryByText('Per blob store')).not.toBeInTheDocument()
  })
})
