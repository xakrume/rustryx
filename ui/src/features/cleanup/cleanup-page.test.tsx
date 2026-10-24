import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetLifecycleMock } from '@/features/lifecycle/mocks'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetCleanupMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number, details?: unknown) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}`, ...(details ? { details } : {}) } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

beforeEach(() => {
  resetCleanupMock()
  resetLifecycleMock()
})

describe('cleanup policy list', () => {
  it('lists the policies with their criteria in words, attachments and last run', async () => {
    const { container } = renderApp('/admin/cleanup-policies', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Cleanup policies' })
    expect(
      await within(table).findByRole('link', { name: 'maven-old-snapshots' }),
    ).toBeInTheDocument()
    expect(within(table).getByText('older than 90 days')).toBeInTheDocument()
    expect(within(table).getByText('pre-release versions only')).toBeInTheDocument()
    expect(within(table).getByText('npm-releases, npm-snapshots')).toBeInTheDocument()
    expect(within(table).getAllByText('No repository').length).toBeGreaterThan(0)
    expect(within(table).getByText(/412 items/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('filters by format in the address bar', async () => {
    renderApp('/admin/cleanup-policies?format=npm', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Cleanup policies' })
    expect(
      await within(table).findByRole('link', { name: 'npm-stale-packages' }),
    ).toBeInTheDocument()
    expect(
      within(table).queryByRole('link', { name: 'maven-old-snapshots' }),
    ).not.toBeInTheDocument()
  })

  it('shows the first use state with a create action', async () => {
    renderApp('/admin/cleanup-policies', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/cleanup-policies', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('No cleanup policies yet')).toBeInTheDocument()
  })

  it('shows the error with code and request id', async () => {
    renderApp('/admin/cleanup-policies', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/cleanup-policies', () => envelope('internal_error', 'Boom', 500)),
      ],
    })
    expect(await screen.findByText('internal_error')).toBeInTheDocument()
    expect(screen.getByText('req-internal_error')).toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/cleanup-policies', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })
})

describe('cleanup policy editor', () => {
  it('creates a policy, then reopens it on the saved version', async () => {
    const user = userEvent.setup()
    const { container } = renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'maven-temp')
    await user.type(screen.getByLabelText(/^Older than/), '30')
    expect(screen.getByText('older than 30 days')).toBeInTheDocument()
    await expectNoA11yViolations(container)
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(await screen.findByText('Policy maven-temp created')).toBeInTheDocument()
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Edit cleanup policy maven-temp' }),
    ).toBeInTheDocument()
    // Saved: the preview is available now.
    expect(screen.getByRole('button', { name: 'Preview what would be removed' })).toBeEnabled()
  })

  it('must_require_at_least_one_criterion_and_a_name', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Save policy' }))
    expect(await screen.findByText('Enter a name.')).toBeInTheDocument()
    expect(screen.getByText(/Set at least one criterion/)).toBeInTheDocument()
    expect(screen.getByLabelText(/^Name/)).toHaveFocus()
  })

  it('validates the numbers next to their fields', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'x')
    await user.type(screen.getByLabelText(/^Older than/), '0')
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(await screen.findByText('Enter a whole number of 1 or more.')).toBeInTheDocument()
  })

  it('disables a criterion the format does not support and says why', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await screen.findByRole('option', { name: 'Raw' })
    await user.selectOptions(screen.getByLabelText(/^Format/), 'raw')
    const lastDownload = screen.getByLabelText(/^Not downloaded for/)
    expect(lastDownload).toBeDisabled()
    expect(screen.getAllByText('Not supported for Raw repositories.').length).toBeGreaterThan(1)
    expect(screen.getByLabelText(/^Release type/)).toBeDisabled()
    expect(screen.getByLabelText(/^Older than/)).toBeEnabled()
  })

  it('maps a pattern the engine refuses onto the pattern field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'bad-pattern')
    await user.type(screen.getByLabelText(/^Component name pattern/), '(a)\\1')
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(
      await screen.findByText('Backreferences and lookaround are not supported'),
    ).toBeInTheDocument()
    expect(screen.getByLabelText(/^Component name pattern/)).toHaveAttribute('aria-invalid', 'true')
  })

  it('says when a name is taken', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'npm-stale-packages')
    await user.type(screen.getByLabelText(/^Older than/), '5')
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(
      await screen.findByText('A cleanup policy with this name already exists'),
    ).toBeInTheDocument()
  })

  it('saves an edit with the ETag', async () => {
    const user = userEvent.setup()
    let ifMatch: string | null = null
    renderApp('/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000001', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/cleanup-policies/:id', ({ request }) => {
          ifMatch = request.headers.get('if-match')
          return undefined
        }),
      ],
    })
    const days = await screen.findByLabelText(/^Older than/)
    expect(days).toHaveValue('90')
    await user.clear(days)
    await user.type(days, '60')
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(await screen.findByText('Policy maven-old-snapshots saved')).toBeInTheDocument()
    expect(ifMatch).toMatch(/^"cleanup-policy-.*-v1"$/)
    await waitFor(() => expect(screen.getByLabelText(/^Older than/)).toHaveValue('60'))
  })

  it('must_show_the_conflict_dialog_when_the_etag_is_stale', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000001', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/cleanup-policies/:id', () =>
          envelope('precondition_failed', 'Changed by someone else', 412),
        ),
      ],
    })
    const days = await screen.findByLabelText(/^Older than/)
    await user.clear(days)
    await user.type(days, '45')
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Changed by someone else')).toBeInTheDocument()
    expect(
      within(dialog).getByRole('button', { name: 'Overwrite with my changes' }),
    ).toBeInTheDocument()
  })

  it('locks the format while repositories use the policy', async () => {
    renderApp('/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000002', {
      user: 'admin',
    })
    const format = await screen.findByLabelText(/^Format/)
    expect(format).toBeDisabled()
    expect(screen.getByText(/Locked: 2 repositories use this policy/)).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'npm-snapshots' })).toBeInTheDocument()
  })
})

describe('cleanup policy dry run preview', () => {
  it('previews what would be removed, with counts, sizes and the reason for each item', async () => {
    const user = userEvent.setup()
    const { container } = renderApp(
      '/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000001',
      {
        user: 'admin',
      },
    )
    await user.click(await screen.findByRole('button', { name: 'Preview what would be removed' }))
    expect(await screen.findByText('Planning what would be removed')).toBeInTheDocument()
    expect(await screen.findByText('Items to remove', {}, { timeout: 5000 })).toBeInTheDocument()
    expect(screen.getByText('Reclaimable size')).toBeInTheDocument()
    expect(screen.getByText('Kept by protection')).toBeInTheDocument()
    const table = await screen.findByRole('region', { name: 'Items of the plan' })
    expect(await within(table).findAllByText(/com\.example\.payments:ledger/)).not.toHaveLength(0)
    expect(within(table).getAllByText('Age').length).toBeGreaterThan(0)
    expect(screen.getByText(/A dry run deletes nothing/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('must_not_offer_the_preview_for_an_unsaved_form', async () => {
    renderApp('/admin/cleanup-policies?edit=new', { user: 'admin' })
    const button = await screen.findByRole('button', { name: 'Preview what would be removed' })
    expect(button).toBeDisabled()
    expect(button).toHaveAccessibleDescription(/Save the policy first/)
  })

  it('must_not_preview_the_saved_version_while_the_form_has_unsaved_changes', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000001', {
      user: 'admin',
    })
    const days = await screen.findByLabelText(/^Older than/)
    const button = screen.getByRole('button', { name: 'Preview what would be removed' })
    expect(button).toBeEnabled()
    await user.type(days, '1')
    expect(button).toBeDisabled()
    expect(button).toHaveAccessibleDescription(/Save your changes first/)
  })

  it('offers the running dry run when one is already active', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies?edit=0192f6c4-5a1e-7000-8002-000000000003', {
      user: 'admin',
    })
    await user.click(await screen.findByRole('button', { name: 'Preview what would be removed' }))
    expect(
      await screen.findByText('A dry run of this policy is already running.'),
    ).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Show the running dry run' }))
    expect(await screen.findByRole('heading', { name: /Dry run report/ })).toBeInTheDocument()
  })
})

describe('cleanup policy delete', () => {
  it('names the repositories that lose the policy and detaches them on confirm', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Delete npm-stale-packages' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/2 repositories use it/)).toBeInTheDocument()
    expect(within(dialog).getAllByText('npm-snapshots').length).toBeGreaterThan(0)
    await user.type(within(dialog).getByLabelText('Confirmation text'), 'npm-stale-packages')
    await user.click(within(dialog).getByRole('button', { name: 'Detach and delete' }))
    expect(await screen.findByText('Policy npm-stale-packages deleted')).toBeInTheDocument()
    const table = await screen.findByRole('region', { name: 'Cleanup policies' })
    await waitFor(() =>
      expect(
        within(table).queryByRole('link', { name: 'npm-stale-packages' }),
      ).not.toBeInTheDocument(),
    )
  })

  it('deletes an unused policy with the plain confirmation', async () => {
    const user = userEvent.setup()
    renderApp('/admin/cleanup-policies', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Delete raw-temp-files' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).queryByText(/repositories use it/)).not.toBeInTheDocument()
    await user.type(within(dialog).getByLabelText('Confirmation text'), 'raw-temp-files')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await screen.findByText('Policy raw-temp-files deleted')).toBeInTheDocument()
  })
})
