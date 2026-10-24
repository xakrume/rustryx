import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetSettingsMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number, details?: unknown) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}`, ...(details ? { details } : {}) } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

beforeEach(() => resetSettingsMock())

describe('system settings', () => {
  it('shows each section as a tab and the general section first', async () => {
    const { container } = renderApp('/admin/settings', { user: 'admin' })
    expect(await screen.findByRole('tab', { name: 'General', selected: true })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Security' })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Storage' })).toBeInTheDocument()
    expect(await screen.findByLabelText(/Read only mode/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('must_not_make_a_field_from_the_configuration_editable', async () => {
    renderApp('/admin/settings?section=security', { user: 'admin' })
    const readOnly = await screen.findByRole('region', { name: 'Read only' })
    // Shown with its source and the way to change it, but there is no input for it.
    expect(
      within(readOnly).getByText('Session idle timeout (seconds)', { exact: false }),
    ).toBeInTheDocument()
    expect(within(readOnly).getByText('1800')).toBeInTheDocument()
    expect(within(readOnly).getAllByText('Configuration').length).toBeGreaterThan(0)
    expect(within(readOnly).getAllByText(/Change it there and restart/).length).toBeGreaterThan(0)
    expect(screen.queryByLabelText(/Session idle timeout/)).not.toBeInTheDocument()
    expect(screen.queryByLabelText(/Session absolute lifetime/)).not.toBeInTheDocument()
    // The editable ones are real inputs.
    expect(screen.getByLabelText(/Default token lifetime/)).toBeEnabled()
  })

  it('must_not_show_a_secret_only_whether_one_is_set', async () => {
    renderApp('/admin/settings?section=security', { user: 'admin' })
    const readOnly = await screen.findByRole('region', { name: 'Read only' })
    expect(within(readOnly).getByText('Configured')).toBeInTheDocument()
    expect(screen.getByText(/Secrets are never shown/)).toBeInTheDocument()
  })

  it('opens the section named in the address bar', async () => {
    renderApp('/admin/settings?section=storage', { user: 'admin' })
    expect(await screen.findByRole('tab', { name: 'Storage', selected: true })).toBeInTheDocument()
    expect(await screen.findByLabelText(/Quota warning level/)).toHaveValue('80')
  })

  it('saves a changed value with the ETag and shows where it comes from now', async () => {
    const user = userEvent.setup()
    let ifMatch: string | null = null
    renderApp('/admin/settings?section=security', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/settings/:section', ({ request }) => {
          ifMatch = request.headers.get('if-match')
          return undefined
        }),
      ],
    })
    const field = await screen.findByLabelText(/Default token lifetime/)
    await user.clear(field)
    await user.type(field, '60')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(await screen.findByText('Settings saved')).toBeInTheDocument()
    expect(ifMatch).toMatch(/^"settings-security-v1"$/)
    await waitFor(() => expect(screen.getByLabelText(/Default token lifetime/)).toHaveValue('60'))
    expect(screen.getByText(/Last changed/)).toBeInTheDocument()
  })

  it('validates the range next to the field before sending anything', async () => {
    const user = userEvent.setup()
    let sent = false
    renderApp('/admin/settings?section=security', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/settings/:section', () => {
          sent = true
          return undefined
        }),
      ],
    })
    const field = await screen.findByLabelText(/Default token lifetime/)
    await user.clear(field)
    await user.type(field, '4000')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(await screen.findByText('Enter a whole number between 1 and 365.')).toBeInTheDocument()
    expect(field).toHaveAttribute('aria-invalid', 'true')
    expect(sent).toBe(false)
  })

  it('maps a refused combination from the server onto the field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/settings?section=security', { user: 'admin' })
    const longest = await screen.findByLabelText(/Longest token lifetime/)
    await user.clear(longest)
    await user.type(longest, '30')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(
      await screen.findByText('The default lifetime must not exceed the longest lifetime'),
    ).toBeInTheDocument()
    expect(screen.getByLabelText(/Default token lifetime/)).toHaveAttribute('aria-invalid', 'true')
  })

  it('must_show_the_conflict_dialog_when_the_etag_is_stale', async () => {
    const user = userEvent.setup()
    renderApp('/admin/settings?section=storage', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/settings/:section', () =>
          envelope('precondition_failed', 'Changed', 412),
        ),
      ],
    })
    const field = await screen.findByLabelText(/Quota warning level/)
    await user.clear(field)
    await user.type(field, '70')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Changed by someone else')).toBeInTheDocument()
  })

  it('explains a field that the configuration owns when the server refuses it with 409', async () => {
    const user = userEvent.setup()
    renderApp('/admin/settings?section=storage', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/settings/:section', () =>
          envelope(
            'setting_managed_by_config',
            'quota_warning_percent comes from the configuration and cannot change at runtime',
            409,
          ),
        ),
      ],
    })
    const field = await screen.findByLabelText(/Quota warning level/)
    await user.clear(field)
    await user.type(field, '75')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/comes from the configuration/)).toBeInTheDocument()
  })

  it('must_ask_before_turning_on_read_only_mode_and_do_nothing_when_declined', async () => {
    const user = userEvent.setup()
    let puts = 0
    renderApp('/admin/settings', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/settings/:section', () => {
          puts += 1
          return undefined
        }),
      ],
    })
    await user.click(await screen.findByRole('switch', { name: /Read only mode/ }))
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Turn on read only mode?')).toBeInTheDocument()
    expect(within(dialog).getByText(/refused with the error read_only_mode/)).toBeInTheDocument()
    // The safe answer has the focus.
    expect(within(dialog).getByRole('button', { name: 'Keep it off' })).toHaveFocus()
    await user.click(within(dialog).getByRole('button', { name: 'Keep it off' }))
    expect(puts).toBe(0)
  })

  it('turns read only mode on after the confirmation', async () => {
    const user = userEvent.setup()
    renderApp('/admin/settings', { user: 'admin' })
    await user.click(await screen.findByRole('switch', { name: /Read only mode/ }))
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.click(within(dialog).getByRole('button', { name: 'Turn on read only mode' }))
    expect(await screen.findByText('Settings saved')).toBeInTheDocument()
    await waitFor(() =>
      expect(screen.getByRole('switch', { name: /Read only mode/ })).toBeChecked(),
    )
  })

  it('turns read only mode off without a question', async () => {
    const user = userEvent.setup()
    renderApp('/admin/settings', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/settings/general', () =>
          HttpResponse.json(
            {
              section: 'general',
              values: { base_url: 'https://repo.example.org', read_only_mode: true },
              sources: { base_url: 'config', read_only_mode: 'runtime' },
              editable_fields: ['read_only_mode'],
              updated_at: null,
              updated_by: null,
            },
            { headers: { ETag: '"settings-general-v1"' } },
          ),
        ),
      ],
    })
    await user.click(await screen.findByRole('switch', { name: /Read only mode/ }))
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(await screen.findByText('Settings saved')).toBeInTheDocument()
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
  })

  it('shows the error with code and request id when the settings cannot be loaded', async () => {
    renderApp('/admin/settings', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/settings/schema', () => envelope('internal_error', 'Boom', 500)),
      ],
    })
    expect(await screen.findByText('The settings could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('req-internal_error')).toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/settings', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })
})
