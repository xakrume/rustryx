import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockId } from '@/features/system/mock-kit'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetRoutingMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number, details?: unknown) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}`, ...(details ? { details } : {}) } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

const BLOCK_INTERNAL = mockId(9, 1)
const FREE_RULE = mockId(9, 3)

beforeEach(() => resetRoutingMock())

describe('routing rule list', () => {
  it('lists the rules with mode in words, patterns and assigned repositories', async () => {
    const { container } = renderApp('/admin/routing-rules', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Routing rules' })
    expect(
      await within(table).findByRole('link', { name: 'block-internal-paths' }),
    ).toBeInTheDocument()
    expect(within(table).getAllByText('Block').length).toBeGreaterThan(0)
    expect(within(table).getByText('Allow only')).toBeInTheDocument()
    expect(within(table).getByText('^/internal/.*')).toBeInTheDocument()
    expect(within(table).getAllByText('+1 more').length).toBeGreaterThan(0)
    expect(within(table).getByText('maven-proxy, npm-proxy')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('shows the first use state', async () => {
    renderApp('/admin/routing-rules', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/routing-rules', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('No routing rules yet')).toBeInTheDocument()
  })

  it('shows the error with code and request id', async () => {
    renderApp('/admin/routing-rules', {
      user: 'admin',
      overrides: [http.get('/api/v1/routing-rules', () => envelope('overloaded', 'Busy', 503))],
    })
    expect(await screen.findByText('overloaded')).toBeInTheDocument()
    expect(screen.getByText('req-overloaded')).toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/routing-rules', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('must_block_the_delete_button_of_a_rule_in_use_and_name_the_reason', async () => {
    renderApp('/admin/routing-rules', { user: 'admin' })
    const blocked = await screen.findByRole('button', {
      name: 'Delete block-internal-paths (blocked: used by 2 repositories)',
    })
    expect(blocked).toBeDisabled()
    expect(
      screen.getByRole('button', { name: 'Delete block-snapshots-from-central' }),
    ).toBeEnabled()
  })
})

describe('routing rule editor', () => {
  it('creates a rule and reopens it on the saved version', async () => {
    const user = userEvent.setup()
    const { container } = renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'block-tmp')
    await user.type(screen.getByLabelText('Pattern 1'), '^/tmp/.*')
    expect(await screen.findByText('All patterns are valid.')).toBeInTheDocument()
    await expectNoA11yViolations(container)
    await user.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(await screen.findByText('Rule block-tmp created')).toBeInTheDocument()
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Edit routing rule block-tmp' }),
    ).toBeInTheDocument()
  })

  it('validates the name and every pattern next to the fields', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Save rule' }))
    expect(await screen.findByText('Enter a name.')).toBeInTheDocument()
    expect(screen.getByText('Enter a pattern, or remove this row.')).toBeInTheDocument()
    expect(screen.getByLabelText(/^Name/)).toHaveFocus()
  })

  it('lints patterns while typing and shows the position of the error', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText('Pattern 1'), '(a)\\1')
    expect(
      await screen.findByText('Position 3: Backreferences and lookaround are not supported'),
    ).toBeInTheDocument()
    expect(screen.getByLabelText('Pattern 1')).toHaveAttribute('aria-invalid', 'true')
  })

  it('adds a pattern row with the keyboard and focuses it, and removes a row', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await screen.findByLabelText('Pattern 1')
    await user.click(screen.getByRole('button', { name: 'Add pattern' }))
    expect(await screen.findByLabelText('Pattern 2')).toHaveFocus()
    await user.click(screen.getByRole('button', { name: 'Remove pattern 2' }))
    await waitFor(() => expect(screen.queryByLabelText('Pattern 2')).not.toBeInTheDocument())
  })

  it('tests paths against the rule in the form: a block rule blocks what matches', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await user.type(await screen.findByLabelText('Pattern 1'), '^/internal/.*')
    await user.type(
      screen.getByLabelText('Paths to test'),
      '/internal/secret.jar{Enter}/public/lib.jar',
    )
    await user.click(screen.getByRole('button', { name: 'Test paths' }))
    const results = await screen.findByRole('region', { name: 'Test results' })
    const rows = within(results).getAllByRole('row')
    expect(within(rows[1]!).getByText('Blocked')).toBeInTheDocument()
    expect(within(rows[1]!).getByText('^/internal/.*')).toBeInTheDocument()
    expect(within(rows[1]!).getByText('matches a block pattern')).toBeInTheDocument()
    expect(within(rows[2]!).getByText('Allowed')).toBeInTheDocument()
    expect(within(rows[2]!).getByText('No pattern matched')).toBeInTheDocument()
  })

  it('must_evaluate_the_mode_of_the_form_not_the_saved_rule', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`, { user: 'admin' })
    await screen.findByLabelText('Pattern 1')
    // The saved rule blocks /internal/...; switch the form to "allow only" without saving.
    await user.click(screen.getByLabelText(/Allow only matching paths/))
    await user.type(screen.getByLabelText('Paths to test'), '/internal/a.jar')
    await user.click(screen.getByRole('button', { name: 'Test paths' }))
    const results = await screen.findByRole('region', { name: 'Test results' })
    expect(within(results).getByText('Allowed')).toBeInTheDocument()
    expect(within(results).getByText('matches an allow pattern')).toBeInTheDocument()
  })

  it('asks for a pattern and a path before testing', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Test paths' }))
    expect(screen.getByText('Add a valid pattern first.')).toBeInTheDocument()
    await user.type(screen.getByLabelText('Pattern 1'), '^/a')
    await user.click(screen.getByRole('button', { name: 'Test paths' }))
    expect(screen.getByText('Enter at least one path.')).toBeInTheDocument()
  })

  it('maps a pattern the server refuses onto its row', async () => {
    const user = userEvent.setup()
    renderApp('/admin/routing-rules?edit=new', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/routing-rules/validate', () =>
          HttpResponse.json({ valid: true, errors: [] }),
        ),
      ],
    })
    await user.type(await screen.findByLabelText(/^Name/), 'refused')
    await user.type(screen.getByLabelText('Pattern 1'), '(?=x)')
    await user.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(
      await screen.findAllByText(/Backreferences and lookaround are not supported/),
    ).not.toHaveLength(0)
    expect(screen.getByLabelText('Pattern 1')).toHaveAttribute('aria-invalid', 'true')
  })

  it('saves an edit with the ETag and shows the saved state', async () => {
    const user = userEvent.setup()
    let ifMatch: string | null = null
    renderApp(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`, {
      user: 'admin',
      overrides: [
        http.put('/api/v1/routing-rules/:id', ({ request }) => {
          ifMatch = request.headers.get('if-match')
          return undefined
        }),
      ],
    })
    const description = await screen.findByLabelText(/^Description/)
    await user.type(description, ' More.')
    await user.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(await screen.findByText('Rule block-internal-paths saved')).toBeInTheDocument()
    expect(ifMatch).toMatch(/^"routing-rule-.*-v1"$/)
  })

  it('must_show_the_conflict_dialog_when_the_etag_is_stale', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`, {
      user: 'admin',
      overrides: [
        http.put('/api/v1/routing-rules/:id', () =>
          envelope('precondition_failed', 'Changed', 412),
        ),
      ],
    })
    await user.type(await screen.findByLabelText(/^Description/), 'x')
    await user.click(screen.getByRole('button', { name: 'Save rule' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Changed by someone else')).toBeInTheDocument()
  })

  it('shows the assigned repositories and blocks deletion with the reason', async () => {
    renderApp(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`, { user: 'admin' })
    expect(await screen.findByRole('link', { name: 'maven-proxy' })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'npm-proxy' })).toBeInTheDocument()
    const del = screen.getByRole('button', { name: 'Delete' })
    expect(del).toBeDisabled()
    expect(screen.getByText('This rule is in use')).toBeInTheDocument()
    expect(del).toHaveAccessibleDescription(/maven-proxy, npm-proxy/)
  })

  it('deletes a rule nobody uses after the typed confirmation', async () => {
    const user = userEvent.setup()
    renderApp(`/admin/routing-rules?edit=${FREE_RULE}`, { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.type(
      within(dialog).getByLabelText('Confirmation text'),
      'block-snapshots-from-central',
    )
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await screen.findByText('Rule block-snapshots-from-central deleted')).toBeInTheDocument()
    const table = await screen.findByRole('region', { name: 'Routing rules' })
    expect(
      within(table).queryByRole('link', { name: 'block-snapshots-from-central' }),
    ).not.toBeInTheDocument()
  })
})
