import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { readBlob } from '@/features/roles/test-support'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const FIRST = () => identity().audit[0]!

describe('audit log page', () => {
  it('lists the events newest first with kind and outcome as text, without accessibility violations', async () => {
    renderApp('/admin/audit', { user: 'admin' })
    const page = await inMain()
    expect(
      (await page.findAllByRole('button', { name: /^Open details of the event/ })).length,
    ).toBeGreaterThan(10)
    expect(page.getAllByText('Success').length).toBeGreaterThan(0)
    expect(page.getByText(/events older than 90 days are dropped by month/i)).toBeInTheDocument()
    const rows = page.getAllByRole('row')
    expect(rows[1]).toHaveTextContent(FIRST().action)
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/audit', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('filters by kind with checkboxes and keeps the filter in the address', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/admin/audit', { user: 'admin' })
    const page = await inMain()
    await user.click(await page.findByRole('checkbox', { name: 'Failed sign in' }))
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ kind: 'login_failure' }),
    )
    await waitFor(() =>
      expect(within(page.getByRole('table')).queryAllByText('Success')).toHaveLength(0),
    )
    expect(page.getAllByText('Failed sign in').length).toBeGreaterThan(1)
    await user.click(page.getByRole('checkbox', { name: 'Token' }))
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ kind: 'login_failure,token_change' }),
    )
    await user.click(page.getByRole('checkbox', { name: 'Failed sign in' }))
    await user.click(page.getByRole('checkbox', { name: 'Token' }))
    await waitFor(() => expect(router.state.location.search).not.toHaveProperty('kind'))
  })

  it('filters by outcome, actor, action and project, and clears them all', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit?outcome=failure&actor=bob&action=auth.login&project=payments', {
      user: 'admin',
    })
    const page = await inMain()
    expect(await page.findByText('No results for these filters')).toBeInTheDocument()
    expect(page.getByRole('list', { name: 'Active filters' })).toHaveTextContent(
      /Project:\s*payments/,
    )
    await user.click(page.getAllByRole('button', { name: 'Clear filters' })[0]!)
    expect(
      (await page.findAllByRole('button', { name: /^Open details of the event/ })).length,
    ).toBeGreaterThan(0)
  })

  it('applies a relative time range and a custom range', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit?range=hour', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByText('No results for these filters')).toBeInTheDocument()
    await user.selectOptions(page.getByLabelText('Time range'), 'custom')
    expect(page.getByLabelText('From')).toBeInTheDocument()
    expect(page.getByLabelText('Until')).toBeInTheDocument()
    await user.type(page.getByLabelText('From'), '2020-01-01T00:00')
    await waitFor(() =>
      expect(page.queryByText('No results for these filters')).not.toBeInTheDocument(),
    )
    await user.selectOptions(page.getByLabelText('Time range'), '')
    expect(page.queryByLabelText('From')).not.toBeInTheDocument()
  })

  it('sorts oldest first on request and loads more pages with keyset tokens', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit', { user: 'admin' })
    const page = await inMain()
    await page.findAllByRole('button', { name: /^Open details/ })
    const before = page.getAllByRole('row')[1]?.textContent
    await user.click(page.getByRole('button', { name: /Sort by Time/ }))
    await waitFor(() => expect(page.getAllByRole('row')[1]?.textContent).not.toBe(before))
    await user.click(page.getByRole('button', { name: 'Load more' }))
    await waitFor(() => expect(page.getAllByRole('row').length).toBeGreaterThan(51))
  })

  it('opens the full event in a drawer from the keyboard, with the JSON detail, and returns focus', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit', { user: 'admin' })
    const page = await inMain()
    const buttons = await page.findAllByRole('button', { name: /^Open details of the event/ })
    buttons[0]!.focus()
    await user.keyboard('{Enter}')
    const drawer = await screen.findByRole('dialog', { name: FIRST().action })
    expect(within(drawer).getByText(FIRST().id)).toBeInTheDocument()
    expect(within(drawer).getByText(FIRST().request_id!)).toBeInTheDocument()
    expect(within(drawer).getByRole('button', { name: 'Copy Request ID' })).toBeInTheDocument()
    expect(within(drawer).getByText('Detail (JSON)')).toBeInTheDocument()
    await expectNoA11yViolations(drawer)
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    await waitFor(() => expect(buttons[0]).toHaveFocus())
  })

  it('shows an event without detail and an event that failed', async () => {
    const failure = identity().audit.find(
      (e) => e.outcome === 'failure' && Object.keys(e.detail).length === 0,
    )
    const target = failure ?? identity().audit.find((e) => e.outcome === 'failure')!
    renderApp(`/admin/audit?event=${target.id}`, { user: 'admin' })
    const drawer = await screen.findByRole('dialog', { name: target.action })
    expect(within(drawer).getByText('Failure')).toBeInTheDocument()
  })

  it('opens an event by its link even when it is not in the loaded list, and says when it is gone', async () => {
    const old = identity().audit.at(-1)!
    renderApp(`/admin/audit?event=${old.id}`, { user: 'admin' })
    expect(await screen.findByRole('dialog', { name: old.action })).toBeInTheDocument()
    renderApp('/admin/audit?event=missing', { user: 'admin' })
    expect(await screen.findByText('audit_event_not_found')).toBeInTheDocument()
  })

  it('shows the error with code and a retry when the list fails', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'audit:503')
    renderApp('/admin/audit', { user: 'admin' })
    expect(await screen.findByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Retry/ })).toBeInTheDocument()
  })

  it('refreshes the list on request', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit', { user: 'admin' })
    const page = await inMain()
    await page.findAllByRole('button', { name: /^Open details/ })
    await user.click(page.getByRole('button', { name: 'Refresh' }))
    expect((await page.findAllByRole('button', { name: /^Open details/ })).length).toBeGreaterThan(
      0,
    )
  })
})

describe('audit export', () => {
  const blobs: Blob[] = []
  let click: typeof HTMLAnchorElement.prototype.click

  beforeEach(() => {
    blobs.length = 0
    URL.createObjectURL = vi.fn((blob: Blob | MediaSource) => {
      blobs.push(blob as Blob)
      return 'blob:audit'
    })
    URL.revokeObjectURL = vi.fn()
    click = HTMLAnchorElement.prototype.click
    HTMLAnchorElement.prototype.click = function noop() {}
  })
  afterEach(() => {
    HTMLAnchorElement.prototype.click = click
    delete (window as { showSaveFilePicker?: unknown }).showSaveFilePicker
  })

  const linesOf = async (): Promise<{ id: string; cursor: string }[]> =>
    (await readBlob(blobs[0]!))
      .trimEnd()
      .split('\n')
      .map((l) => JSON.parse(l) as { id: string; cursor: string })

  it('streams the matching events as NDJSON, oldest first, and says how many were saved', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit?outcome=failure', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Export NDJSON' }))
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    expect(within(dialog).getByText('outcome: failure')).toBeInTheDocument()
    await expectNoA11yViolations(dialog)
    await user.click(within(dialog).getByRole('button', { name: 'Start export' }))
    const failures = identity().audit.filter((e) => e.outcome === 'failure')
    expect(
      await within(dialog).findByText(
        `Export finished: ${failures.length} events saved as audit-events.ndjson.`,
      ),
    ).toBeInTheDocument()
    const lines = await linesOf()
    expect(lines).toHaveLength(failures.length)
    expect(lines.every((l) => failures.some((f) => f.id === l.id))).toBe(true)
    expect(within(dialog).getByRole('button', { name: 'Export again' })).toBeInTheDocument()
  })

  it('says every event in the range is exported when no other filter is set', async () => {
    renderApp('/admin/audit?export=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    expect(within(dialog).getByText(/every event in the range is exported/)).toBeInTheDocument()
  })

  it('must_resume_after_the_last_complete_event_when_the_connection_breaks', async () => {
    const user = userEvent.setup()
    window.sessionStorage.setItem('ryx-mock-chaos', 'audit-export:cut')
    renderApp('/admin/audit?export=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    await user.click(within(dialog).getByRole('button', { name: 'Start export' }))
    expect(
      await within(dialog).findByText(/The export was interrupted after \d+ events/),
    ).toBeInTheDocument()
    expect(blobs).toHaveLength(0)
    await user.click(within(dialog).getByRole('button', { name: 'Resume' }))
    expect(await within(dialog).findByText(/Export finished: \d+ events saved/)).toBeInTheDocument()
    const lines = await linesOf()
    // Whole events only, none twice, none missing.
    expect(new Set(lines.map((l) => l.id)).size).toBe(lines.length)
    expect(lines).toHaveLength(identity().audit.length)
  })

  it('checks the range before asking the server', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit?export=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    await user.type(within(dialog).getByLabelText('From'), '2024-01-01T00:00')
    await user.type(within(dialog).getByLabelText('Until'), '2026-06-01T00:00')
    expect(
      within(dialog).getByText('The range is longer than 366 days. Choose a shorter one.'),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Start export' })).toBeDisabled()
    await user.clear(within(dialog).getByLabelText('Until'))
    await user.type(within(dialog).getByLabelText('Until'), '2023-01-01T00:00')
    expect(within(dialog).getByText('Choose a start before the end.')).toBeInTheDocument()
  })

  it('shows what the server refused and writes nothing', async () => {
    const user = userEvent.setup()
    renderApp('/admin/audit?export=1', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/audit-events/export', () =>
          HttpResponse.json(
            { error: { code: 'range_too_large', message: 'Too wide', request_id: 'req-r' } },
            { status: 400, headers: { 'X-Request-Id': 'req-r' } },
          ),
        ),
      ],
    })
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    await user.click(within(dialog).getByRole('button', { name: 'Start export' }))
    expect(await within(dialog).findByText('range_too_large')).toBeInTheDocument()
    expect(within(dialog).getByText('req-r')).toBeInTheDocument()
    expect(blobs).toHaveLength(0)
  })

  it('writes to a file the user picked when the browser can, and tells when the dialog was closed', async () => {
    const user = userEvent.setup()
    const written: string[] = []
    const writable = Object.assign(
      new WritableStream<Uint8Array>({
        write: (c) => void written.push(new TextDecoder().decode(c)),
      }),
      { close: () => Promise.resolve(), abort: () => Promise.resolve() },
    )
    const picker = vi
      .fn()
      .mockRejectedValueOnce(new DOMException('closed', 'AbortError'))
      .mockResolvedValue({ createWritable: () => Promise.resolve(writable) })
    ;(window as { showSaveFilePicker?: unknown }).showSaveFilePicker = picker
    renderApp('/admin/audit?export=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Export audit events' })
    await user.click(within(dialog).getByRole('button', { name: 'Start export' }))
    expect(
      await within(dialog).findByText('The save dialog was closed. Nothing was exported.'),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Start export' }))
    expect(await within(dialog).findByText(/Export finished/)).toBeInTheDocument()
    expect(written.join('').trimEnd().split('\n')).toHaveLength(identity().audit.length)
    expect(blobs).toHaveLength(0)
  })
})
