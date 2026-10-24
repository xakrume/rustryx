import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { meBody } from '@/mocks/session'
import { readSession } from '@/mocks/session'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const NEW_PASSWORD = 'a brand new password 1'

describe('profile page', () => {
  it('shows who the administrator is, the roles with their origin and the permission summary', async () => {
    renderApp('/me', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('heading', { level: 1, name: 'Profile' })).toBeInTheDocument()
    expect(page.getAllByText('Administrator').length).toBeGreaterThan(0)
    expect(page.getByText('admin@example.org')).toBeInTheDocument()
    expect(page.getByText('Browser session')).toBeInTheDocument()
    expect(page.getByText('Manual')).toBeInTheDocument()
    expect(page.getByText(/You hold nx-admin/)).toBeInTheDocument()
    expect(page.getByText('Audit log')).toBeInTheDocument()
    expect(page.getByText('nx-repository-view-*-*')).toBeInTheDocument()
    expect(page.getByText('Administer Rustryx')).toBeInTheDocument()
    expect(page.getByText('Local accounts have no groups.')).toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('shows an SSO account with roles from groups, the groups seen and no password form', async () => {
    renderApp('/me', { user: 'alice' })
    const page = await inMain()
    expect(await page.findAllByText('OIDC group')).toHaveLength(2)
    expect(page.getByText('Manual')).toBeInTheDocument()
    expect(page.getAllByText('/rustryx/team-payments/publishers').length).toBeGreaterThan(1)
    expect(page.getByText(/Confirmed by the identity provider/)).toBeInTheDocument()
    expect(page.getByText('https://sso.example.org/realms/rustryx')).toBeInTheDocument()
    expect(page.getByText('Your password is managed elsewhere')).toBeInTheDocument()
    expect(page.queryByLabelText(/^Current password/)).not.toBeInTheDocument()
    expect(page.getByText(/You are not an administrator/)).toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('explains what to do when the account holds no roles', async () => {
    renderApp('/me', {
      user: 'alice',
      overrides: [
        http.get('/api/v1/me', () =>
          HttpResponse.json({ ...meBody(readSession()), roles: [], groups: [] }),
        ),
      ],
    })
    const page = await inMain()
    expect(await page.findByText('You hold no roles.')).toBeInTheDocument()
    expect(page.getByText(/Ask an administrator/)).toBeInTheDocument()
    expect(page.getByText('No groups were reported for this sign in.')).toBeInTheDocument()
  })

  it('shows the token and its scopes when the request is authenticated by an API token', async () => {
    renderApp('/me', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/me', () =>
          HttpResponse.json({
            ...meBody(readSession()),
            auth_method: 'api_token',
            token: { id: 't1', name: 'ci-publisher', scopes: ['repo:maven-releases:write'] },
          }),
        ),
      ],
    })
    const page = await inMain()
    expect(await page.findAllByText('API token')).toHaveLength(2)
    expect(page.getByText('ci-publisher')).toBeInTheDocument()
    expect(page.getByText(/write on maven-releases/)).toBeInTheDocument()
  })

  it('sends an anonymous visitor to the sign in page', async () => {
    const { router } = renderApp('/me')
    await waitFor(() => expect(router.state.location.pathname).toBe('/login'))
  })

  it('must_not_keep_any_password_on_the_page_after_a_successful_change', async () => {
    const user = userEvent.setup()
    renderApp('/me', { user: 'admin' })
    const page = await inMain()
    await user.type(await page.findByLabelText(/^Current password/), 'mock-password')
    await user.type(page.getByLabelText(/^New password/), NEW_PASSWORD)
    await user.type(page.getByLabelText(/^Repeat the new password/), NEW_PASSWORD)
    await user.click(page.getByRole('button', { name: 'Change password' }))
    expect(
      await screen.findByText('Password changed. Your other sessions ended.'),
    ).toBeInTheDocument()
    expect(identity().users.find((u) => u.username === 'admin')?.password).toBe(NEW_PASSWORD)
    for (const label of [/^Current password/, /^New password/, /^Repeat the new password/]) {
      expect(page.getByLabelText(label)).toHaveValue('')
    }
    expect(screen.queryByDisplayValue(NEW_PASSWORD)).not.toBeInTheDocument()
    expect(document.body.textContent).not.toContain(NEW_PASSWORD)
  })

  it('says the current password is wrong at that field and clears every password field', async () => {
    const user = userEvent.setup()
    renderApp('/me', { user: 'admin' })
    const page = await inMain()
    await user.type(await page.findByLabelText(/^Current password/), 'not the password')
    await user.type(page.getByLabelText(/^New password/), NEW_PASSWORD)
    await user.type(page.getByLabelText(/^Repeat the new password/), NEW_PASSWORD)
    await user.click(page.getByRole('button', { name: 'Change password' }))
    expect(await page.findByText('The current password is not correct.')).toBeInTheDocument()
    expect(page.getByLabelText(/^New password/)).toHaveValue('')
    await waitFor(() => expect(page.getByLabelText(/^Current password/)).toHaveFocus())
    expect(identity().users.find((u) => u.username === 'admin')?.password).toBe('mock-password')
  })

  it('validates the new password before any request', async () => {
    const user = userEvent.setup()
    renderApp('/me', { user: 'admin' })
    const page = await inMain()
    await user.click(await page.findByRole('button', { name: 'Change password' }))
    expect(await page.findByText('Enter your current password.')).toBeInTheDocument()
    expect(page.getByText('The new password needs at least 12 characters.')).toBeInTheDocument()
    await user.type(page.getByLabelText(/^New password/), 'ryx_this is long enough')
    await user.type(page.getByLabelText(/^Repeat the new password/), 'something different')
    await user.click(page.getByRole('button', { name: 'Change password' }))
    expect(await page.findByText('A password must not start with ryx_.')).toBeInTheDocument()
    expect(page.getByText('The passwords do not match.')).toBeInTheDocument()
  })

  it('shows code and request id when the change is throttled', async () => {
    const user = userEvent.setup()
    renderApp('/me', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/me/password', () =>
          HttpResponse.json(
            { error: { code: 'locked_out', message: 'Too many attempts', request_id: 'req-lock' } },
            { status: 429, headers: { 'Retry-After': '30', 'X-Request-Id': 'req-lock' } },
          ),
        ),
      ],
    })
    const page = await inMain()
    await user.type(await page.findByLabelText(/^Current password/), 'mock-password')
    await user.type(page.getByLabelText(/^New password/), NEW_PASSWORD)
    await user.type(page.getByLabelText(/^Repeat the new password/), NEW_PASSWORD)
    await user.click(page.getByRole('button', { name: 'Change password' }))
    expect(await screen.findByText('locked_out')).toBeInTheDocument()
    expect(screen.getByText('req-lock')).toBeInTheDocument()
  })

  it('signs out everywhere after a confirmation and lands on the sign in page', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/me', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Sign out everywhere' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Sign out everywhere?' })
    expect(dialog).toHaveAccessibleDescription(/API tokens keep working/)
    await user.click(within(dialog).getByRole('button', { name: 'Sign out everywhere' }))
    await waitFor(() => expect(router.state.location.pathname).toBe('/login'))
    expect(readSession()).toBeNull()
  })

  it('keeps the dialog and shows the error when ending the sessions fails', async () => {
    const user = userEvent.setup()
    renderApp('/me', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/me/actions/sign-out-everywhere', () =>
          HttpResponse.json(
            { error: { code: 'store_unavailable', message: 'down' } },
            { status: 503 },
          ),
        ),
      ],
    })
    await user.click(await (await inMain()).findByRole('button', { name: 'Sign out everywhere' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Sign out everywhere?' })
    await user.click(within(dialog).getByRole('button', { name: 'Sign out everywhere' }))
    expect(await within(dialog).findByText('store_unavailable')).toBeInTheDocument()
  })
})
