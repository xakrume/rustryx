import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockId } from '@/features/system/mock-kit'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetTasksMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}` } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

const taskId = (n: number) => mockId(7, n)
const runId = (taskN: number, i: number) => mockId(8, taskN * 100 + i)

beforeEach(() => resetTasksMock())

describe('task list', () => {
  it('lists the tasks with state, schedule, last result, duration and next run', async () => {
    const { container } = renderApp('/admin/tasks', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Tasks' })
    expect(
      await within(table).findByRole('link', { name: 'Open task Blob garbage collection' }),
    ).toBeInTheDocument()
    expect(within(table).getAllByText('Every day').length).toBeGreaterThan(0)
    expect(within(table).getByText('Cron 0 2 * * * (UTC)')).toBeInTheDocument()
    expect(within(table).getAllByText('Succeeded').length).toBeGreaterThan(0)
    expect(within(table).getByText('Failed')).toBeInTheDocument()
    expect(within(table).getByText('Running')).toBeInTheDocument()
    expect(within(table).getByText('Aborted')).toBeInTheDocument()
    expect(within(table).getByText('Never run')).toBeInTheDocument()
    expect(within(table).getByText('Disabled')).toBeInTheDocument()
    expect(within(table).getByText('Held by rustryx-0')).toBeInTheDocument()
    expect(within(table).getByText('4 min')).toBeInTheDocument()
    expect(within(table).getAllByText('System').length).toBeGreaterThan(0)
    await expectNoA11yViolations(container)
  })

  it('must_not_offer_any_task_changing_action_while_tasks_are_read_only', async () => {
    renderApp('/admin/tasks', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Tasks' })
    await within(table).findAllByText('Every day')
    for (const label of [
      /^Run now/,
      /^Stop/,
      /^Edit/,
      /^Delete/,
      /^Enable/,
      /^Disable$/,
      /^Create/,
    ]) {
      expect(screen.queryByRole('button', { name: label })).not.toBeInTheDocument()
    }
    expect(screen.getByText(/Tasks are read only in this version/)).toBeInTheDocument()
  })

  it('filters by scope and shows the filtered empty state for user tasks', async () => {
    renderApp('/admin/tasks?scope=user', { user: 'admin' })
    expect(await screen.findByText('No results for these filters')).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Clear filters' }).length).toBeGreaterThan(0)
  })

  it('shows the error with code and request id', async () => {
    renderApp('/admin/tasks', {
      user: 'admin',
      overrides: [http.get('/api/v1/tasks', () => envelope('store_unavailable', 'Down', 503))],
    })
    expect(await screen.findByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-store_unavailable')).toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/tasks', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })
})

describe('task detail', () => {
  it('shows the task, its next runs and its lock', async () => {
    const { container } = renderApp(`/admin/tasks?task=${taskId(1)}`, { user: 'admin' })
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Blob garbage collection' }),
    ).toBeInTheDocument()
    expect(await screen.findByText('Next runs')).toBeInTheDocument()
    expect(screen.getByText('delete_untagged')).toBeInTheDocument()
    expect(screen.getByText(/node rustryx-0 holds its lock/)).toBeInTheDocument()
    expect(
      await screen.findByRole('region', { name: 'Runs of Blob garbage collection' }),
    ).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('lists the run history and filters it by status', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/tasks?task=${taskId(1)}`, { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Runs of Blob garbage collection' })
    await waitFor(() => expect(within(table).getAllByText('Succeeded').length).toBeGreaterThan(3))
    await user.selectOptions(screen.getByLabelText('Status'), 'failed')
    await waitFor(() => expect(within(table).queryByText('Succeeded')).not.toBeInTheDocument())
    expect(within(table).getAllByText('Failed').length).toBeGreaterThan(0)
  })

  it('shows a clear empty state for a task that never ran', async () => {
    renderApp(`/admin/tasks?task=${taskId(6)}`, { user: 'admin' })
    expect(await screen.findByText('This task has not run yet.')).toBeInTheDocument()
  })

  it('opens a run from the history', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/tasks?task=${taskId(4)}`, { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Runs of Expired token purge' })
    const link = (await within(table).findAllByRole('link', { name: /^Open run from/ }))[0]!
    await user.click(link)
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Run of Expired token purge' }),
    ).toBeInTheDocument()
  })

  it('shows not found for an unknown task', async () => {
    renderApp('/admin/tasks?task=nope', { user: 'admin' })
    expect(await screen.findByText('task_not_found')).toBeInTheDocument()
  })
})

describe('task run page', () => {
  it('shows status, timing, counters and the log of a finished run', async () => {
    const { container } = renderApp(`/admin/tasks/${taskId(1)}/runs/${runId(1, 0)}`, {
      user: 'admin',
    })
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Run of Blob garbage collection' }),
    ).toBeInTheDocument()
    expect(await screen.findByText('blobs_removed')).toBeInTheDocument()
    const log = await screen.findByRole('log', { name: 'Log of the run' })
    expect(await within(log).findByText('Starting blob-gc')).toBeInTheDocument()
    expect(within(log).getAllByText('INFO').length).toBeGreaterThan(0)
    expect(screen.getByText(/End of the log/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('names the error code of a failed run and shows the error line in the log', async () => {
    renderApp(`/admin/tasks/${taskId(2)}/runs/${runId(2, 0)}`, { user: 'admin' })
    expect(await screen.findByText('The run failed')).toBeInTheDocument()
    expect(screen.getByText('Error code: blob_store_unavailable')).toBeInTheDocument()
    const log = await screen.findByRole('log', { name: 'Log of the run' })
    expect(await within(log).findByText('Run failed: blob_store_unavailable')).toBeInTheDocument()
    expect(within(log).getByText('ERROR')).toBeInTheDocument()
  })

  it('explains an aborted run', async () => {
    renderApp(`/admin/tasks/${taskId(3)}/runs/${runId(3, 0)}`, { user: 'admin' })
    expect(await screen.findByText('The run was aborted')).toBeInTheDocument()
    expect(screen.getByText(/lost its lock or its node stopped/)).toBeInTheDocument()
  })

  it('follows a running run until it ends', async () => {
    renderApp(`/admin/tasks/${taskId(5)}/runs/${runId(5, 0)}`, { user: 'admin' })
    expect(await screen.findByText(/The run is in progress/)).toBeInTheDocument()
    expect(await screen.findByText(/End of the log/, {}, { timeout: 5000 })).toBeInTheDocument()
    expect(screen.queryByText(/The run is in progress/)).not.toBeInTheDocument()
  })

  it('filters the log by level', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/tasks/${taskId(1)}/runs/${runId(1, 0)}`, { user: 'admin' })
    const log = await screen.findByRole('log', { name: 'Log of the run' })
    await within(log).findByText('Starting blob-gc')
    await user.selectOptions(screen.getByLabelText('Level'), 'warn')
    expect(within(log).queryByText('Starting blob-gc')).not.toBeInTheDocument()
    expect(within(log).getAllByText('WARN').length).toBeGreaterThan(0)
  })

  it('says when the log was removed by retention', async () => {
    renderApp(`/admin/tasks/${taskId(1)}/runs/${runId(1, 35)}`, { user: 'admin' })
    expect(await screen.findByText(/The log was removed by retention/)).toBeInTheDocument()
  })

  it('shows the error with code and request id when the run cannot be loaded', async () => {
    renderApp(`/admin/tasks/${taskId(1)}/runs/${runId(1, 0)}`, {
      user: 'admin',
      overrides: [
        http.get('/api/v1/tasks/:id/runs/:run', () => envelope('internal_error', 'Boom', 500)),
      ],
    })
    expect(await screen.findByText('The run could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('req-internal_error')).toBeInTheDocument()
  })

  it('shows not found for an unknown run', async () => {
    renderApp(`/admin/tasks/${taskId(1)}/runs/${mockId(8, 99999)}`, { user: 'admin' })
    expect(await screen.findByText('task_run_not_found')).toBeInTheDocument()
  })
})
