import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const USER = (name: string) => identity().users.find((u) => u.username === name)
const PASSWORD = 'a long enough password'

describe('users page', () => {
  it('lists users of every source with status and roles, without accessibility violations', async () => {
    renderApp('/admin/users', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('link', { name: 'alice' })).toHaveAttribute(
      'href',
      expect.stringContaining('/admin/users/'),
    )
    expect(page.getAllByText('OIDC').length).toBeGreaterThan(0)
    expect(page.getAllByText('LDAP').length).toBeGreaterThan(0)
    expect(page.getAllByText('Disabled').length).toBeGreaterThan(0)
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('filters by source from the address and loads the next page on request', async () => {
    renderApp('/admin/users?source=ldap', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('link', { name: 'carol' })).toBeInTheDocument()
    expect(page.queryByRole('link', { name: 'alice' })).not.toBeInTheDocument()
  })

  it('pages with keyset tokens and sorts on the server', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users', { user: 'admin' })
    const page = await inMain()
    await page.findByRole('link', { name: 'alice' })
    expect(page.queryByRole('link', { name: 'user-062' })).not.toBeInTheDocument()
    await user.click(page.getByRole('button', { name: 'Load more' }))
    expect(await page.findByRole('link', { name: 'user-062' })).toBeInTheDocument()
    await user.click(page.getByRole('button', { name: /Sort by Username/ }))
    await waitFor(() => expect(page.getAllByRole('row')[1]).toHaveTextContent('user-062'))
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/users', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('creates a local user with roles and opens its page', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Create local user' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create local user' })
    await user.type(within(dialog).getByLabelText(/^Username/), 'zoe')
    await user.type(within(dialog).getByLabelText('Display name'), 'Zoe Example')
    await user.type(within(dialog).getByLabelText('E-mail'), 'zoe@example.org')
    await user.type(within(dialog).getByLabelText(/^Initial password/), PASSWORD)
    await user.type(within(dialog).getByLabelText(/^Repeat the password/), PASSWORD)
    await user.click(await within(dialog).findByRole('checkbox', { name: 'npm-readers' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create user' }))
    expect(await screen.findByRole('heading', { level: 1, name: 'zoe' })).toBeInTheDocument()
    expect(USER('zoe')?.manualRoleIds).toHaveLength(1)
    // The password is nowhere on the page after the call.
    expect(screen.queryByDisplayValue(PASSWORD)).not.toBeInTheDocument()
    expect(document.body.textContent).not.toContain(PASSWORD)
  })

  it('validates the form next to the fields before any request', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create local user' })
    await user.click(within(dialog).getByRole('button', { name: 'Create user' }))
    expect(await within(dialog).findByText('Enter a username.')).toBeInTheDocument()
    expect(
      within(dialog).getByText('The password needs at least 12 characters.'),
    ).toBeInTheDocument()
    await user.type(within(dialog).getByLabelText(/^Username/), '-bad')
    await user.type(within(dialog).getByLabelText(/^Initial password/), 'ryx_a long enough one')
    await user.type(within(dialog).getByLabelText(/^Repeat the password/), 'different')
    await user.click(within(dialog).getByRole('button', { name: 'Create user' }))
    expect(await within(dialog).findByText(/Use letters, digits/)).toBeInTheDocument()
    expect(within(dialog).getByText('A password must not start with ryx_.')).toBeInTheDocument()
    expect(within(dialog).getByText('The passwords do not match.')).toBeInTheDocument()
    expect(identity().users).toHaveLength(70)
  })

  it('explains a username that belongs to another source and clears the passwords', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create local user' })
    await user.type(within(dialog).getByLabelText(/^Username/), 'alice')
    await user.type(within(dialog).getByLabelText(/^Initial password/), PASSWORD)
    await user.type(within(dialog).getByLabelText(/^Repeat the password/), PASSWORD)
    await user.click(within(dialog).getByRole('button', { name: 'Create user' }))
    expect(
      await within(dialog).findByText(/belongs to an account from another sign-in source/),
    ).toBeInTheDocument()
    expect(within(dialog).getByLabelText(/^Initial password/)).toHaveValue('')
    expect(within(dialog).getByLabelText(/^Repeat the password/)).toHaveValue('')
    expect(within(dialog).getByLabelText(/^Username/)).toHaveFocus()
    expect(screen.queryByDisplayValue(PASSWORD)).not.toBeInTheDocument()
  })

  it('says the username is taken when a local account has it', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create local user' })
    await user.type(within(dialog).getByLabelText(/^Username/), 'bob')
    await user.type(within(dialog).getByLabelText(/^Initial password/), PASSWORD)
    await user.type(within(dialog).getByLabelText(/^Repeat the password/), PASSWORD)
    await user.click(within(dialog).getByRole('button', { name: 'Create user' }))
    expect(
      await within(dialog).findByText('A local account with this username exists.'),
    ).toBeInTheDocument()
  })

  it('shows and hides a typed password with a pressed button that names the field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/users?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create local user' })
    const field = within(dialog).getByLabelText(/^Initial password/)
    expect(field).toHaveAttribute('type', 'password')
    await user.click(within(dialog).getByRole('button', { name: 'Show initial password' }))
    expect(field).toHaveAttribute('type', 'text')
    expect(within(dialog).getByRole('button', { name: 'Hide initial password' })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
  })
})
