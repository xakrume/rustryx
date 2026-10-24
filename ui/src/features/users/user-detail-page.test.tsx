import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { ROLE_ID } from '@/features/roles/mock-seed'
import { commit, identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const USER = (name: string) => identity().users.find((u) => u.username === name)!
const PATH = (name: string) => `/admin/users/${USER(name).id}`
const PASSWORD = 'another long password'

describe('user detail page', () => {
  it('shows an external account with roles and their origin, groups and the read only reason', async () => {
    renderApp(PATH('alice'), { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('heading', { level: 1, name: 'alice' })).toBeInTheDocument()
    expect(page.getByText('Managed by the identity provider')).toBeInTheDocument()
    expect(page.getAllByText('OIDC group').length).toBe(2)
    expect(page.getByText('Manual')).toBeInTheDocument()
    expect(
      page.getByText('/rustryx/team-payments/publishers, /rustryx/team-payments'),
    ).toBeInTheDocument()
    expect(page.getByText('https://sso.example.org/realms/rustryx')).toBeInTheDocument()
    // No password reset and no profile editing for an account of the identity provider.
    expect(page.queryByRole('button', { name: 'Reset password' })).not.toBeInTheDocument()
    expect(page.queryByRole('button', { name: 'Edit profile' })).not.toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('lists the tokens of the account with a link to the filtered token list', async () => {
    renderApp(PATH('alice'), { user: 'admin' })
    const page = await inMain()
    const link = await page.findByRole('link', { name: 'View all tokens of this user' })
    expect(link.getAttribute('href')).toContain(`owner_id=${USER('alice').id}`)
    expect(await page.findAllByText('Active')).not.toHaveLength(0)
  })

  it('shows the not found page for an unknown id with the error code', async () => {
    renderApp('/admin/users/00000000-0000-0000-0000-000000000000', { user: 'admin' })
    expect(await screen.findByText('user_not_found')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'All users' })).toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp(PATH('bob'), { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('resets a local password and leaves no password on the page', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Reset password' }))
    const dialog = await screen.findByRole('dialog', { name: 'Reset password of bob' })
    await user.type(within(dialog).getByLabelText(/^New password/), PASSWORD)
    await user.type(
      within(dialog).getByLabelText(/^Repeat the new password/),
      'something else entirely',
    )
    await user.click(within(dialog).getByRole('button', { name: 'Reset password' }))
    expect(await within(dialog).findByText('The passwords do not match.')).toBeInTheDocument()
    await user.clear(within(dialog).getByLabelText(/^Repeat the new password/))
    await user.type(within(dialog).getByLabelText(/^Repeat the new password/), PASSWORD)
    await user.click(within(dialog).getByRole('button', { name: 'Reset password' }))
    expect(await screen.findByText('Password of bob reset')).toBeInTheDocument()
    expect(USER('bob').password).toBe(PASSWORD)
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(screen.queryByDisplayValue(PASSWORD)).not.toBeInTheDocument()
    expect(document.body.textContent).not.toContain(PASSWORD)
  })

  it('disables and enables an account after a confirmation that names it', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Disable user' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Disable user bob?' })
    expect(dialog).toHaveAccessibleDescription(/sessions end at once/)
    await user.click(within(dialog).getByRole('button', { name: 'Disable user' }))
    expect(await screen.findByText('User bob disabled')).toBeInTheDocument()
    expect(USER('bob').enabled).toBe(false)
    await user.click(await (await inMain()).findByRole('button', { name: 'Enable user' }))
    expect(await screen.findByText('User bob enabled')).toBeInTheDocument()
    expect(USER('bob').enabled).toBe(true)
  })

  it('must_not_let_an_administrator_disable_or_delete_the_own_account', async () => {
    renderApp(PATH('admin'), { user: 'admin' })
    const page = await inMain()
    expect(await page.findByText(/This is your own account/)).toBeInTheDocument()
    expect(page.getByRole('button', { name: 'Disable user' })).toBeDisabled()
    expect(page.getByRole('button', { name: 'Delete user' })).toBeDisabled()
  })

  it('deletes an account after the user typed its name', async () => {
    const user = userEvent.setup()
    const { router } = renderApp(PATH('erin'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Delete user' }))
    const dialog = await screen.findByRole('alertdialog', { name: /Delete user\s+erin\s*\?/ })
    const confirm = within(dialog).getByRole('button', { name: 'Delete user' })
    expect(confirm).toBeDisabled()
    await user.type(within(dialog).getByRole('textbox', { name: 'Confirmation text' }), 'erin')
    await user.click(confirm)
    await waitFor(() => expect(router.state.location.pathname).toBe('/admin/users'))
    expect(identity().users.some((u) => u.username === 'erin')).toBe(false)
  })

  it('ends every session of the user', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Sign out everywhere' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Sign bob out everywhere?' })
    await user.click(within(dialog).getByRole('button', { name: 'Sign out everywhere' }))
    expect(await screen.findByText('Sessions of bob ended')).toBeInTheDocument()
    expect(USER('bob').tokensValidAfter).not.toBeNull()
  })

  it('edits the manual roles and keeps the roles from groups out of the picker', async () => {
    const user = userEvent.setup()
    renderApp(PATH('alice'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit manual roles' }))
    const dialog = await screen.findByRole('dialog', { name: 'Manual roles of alice' })
    expect(within(dialog).getByText('Granted by groups (not editable here)')).toBeInTheDocument()
    await user.click(await within(dialog).findByRole('checkbox', { name: 'ci-bots' }))
    await user.click(within(dialog).getByRole('button', { name: 'Save roles' }))
    expect(await screen.findByText('Roles of alice saved')).toBeInTheDocument()
    expect(USER('alice').manualRoleIds).toContain(ROLE_ID.ciBots)
    expect(USER('alice').external).toHaveLength(2)
  })

  it('does not offer nx-admin to an external account, and says why', async () => {
    renderApp(PATH('alice') + '?x=1', { user: 'admin' })
    const user = userEvent.setup()
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit manual roles' }))
    const dialog = await screen.findByRole('dialog', { name: 'Manual roles of alice' })
    const admin = await within(dialog).findByRole('checkbox', { name: 'nx-admin' })
    expect(admin).toBeDisabled()
    expect(admin).toHaveAccessibleDescription(/local accounts only/)
    expect(within(dialog).getByRole('checkbox', { name: 'nx-anonymous' })).toBeDisabled()
  })

  it('must_warn_and_block_removing_nx_admin_from_the_last_local_administrator', async () => {
    const user = userEvent.setup()
    // Only bob is an administrator; the signed in administrator is someone else's problem here.
    commit({
      users: identity().users.map((u) =>
        u.username === 'bob'
          ? { ...u, manualRoleIds: [ROLE_ID.admin] }
          : u.username === 'admin'
            ? { ...u, manualRoleIds: [] }
            : u,
      ),
    })
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit manual roles' }))
    const dialog = await screen.findByRole('dialog', { name: 'Manual roles of bob' })
    await user.click(await within(dialog).findByRole('checkbox', { name: 'nx-admin' }))
    expect(
      await within(dialog).findByText(/This is the last local administrator/),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save roles' })).toBeDisabled()
    // Putting nx-admin back lifts the block.
    await user.click(within(dialog).getByRole('checkbox', { name: 'nx-admin' }))
    expect(within(dialog).queryByText(/last local administrator/)).not.toBeInTheDocument()
  })

  it('must_not_let_an_administrator_remove_nx_admin_from_the_account_in_use', async () => {
    const user = userEvent.setup()
    commit({
      users: identity().users.map((u) =>
        u.username === 'bob' ? { ...u, manualRoleIds: [ROLE_ID.admin] } : u,
      ),
    })
    renderApp(PATH('admin'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit manual roles' }))
    const dialog = await screen.findByRole('dialog', { name: 'Manual roles of admin' })
    await user.click(await within(dialog).findByRole('checkbox', { name: 'nx-admin' }))
    expect(await within(dialog).findByText(/account in use/)).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save roles' })).toBeDisabled()
  })

  it('must_show_the_conflict_dialog_when_the_roles_changed_after_the_page_was_read', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit manual roles' }))
    const dialog = await screen.findByRole('dialog', { name: 'Manual roles of bob' })
    await user.click(await within(dialog).findByRole('checkbox', { name: 'ci-bots' }))
    commit({
      users: identity().users.map((u) => (u.username === 'bob' ? { ...u, manualRoleIds: [] } : u)),
    })
    await user.click(within(dialog).getByRole('button', { name: 'Save roles' }))
    const conflict = await screen.findByRole('alertdialog', { name: 'Changed by someone else' })
    expect(conflict).toHaveTextContent('user bob')
    await user.click(within(conflict).getByRole('button', { name: 'Overwrite with my changes' }))
    expect(await screen.findByText('Roles of bob saved')).toBeInTheDocument()
    expect(USER('bob').manualRoleIds).toHaveLength(3)
    expect(USER('bob').manualRoleIds).toContain(ROLE_ID.ciBots)
  })

  it('edits the display name of a local account with a merge patch of the changed field only', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit profile' }))
    const dialog = await screen.findByRole('dialog', { name: 'Edit profile of bob' })
    const name = within(dialog).getByLabelText('Display name')
    await user.clear(name)
    await user.type(name, 'Robert')
    await user.click(within(dialog).getByRole('button', { name: 'Save profile' }))
    expect(await screen.findByText('Profile of bob saved')).toBeInTheDocument()
    expect(USER('bob').display_name).toBe('Robert')
    expect(USER('bob').email).toBe('bob@example.org')
  })

  it('closes the profile dialog without a request when nothing changed', async () => {
    const user = userEvent.setup()
    renderApp(PATH('bob'), { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Edit profile' }))
    const dialog = await screen.findByRole('dialog', { name: 'Edit profile of bob' })
    await user.click(within(dialog).getByRole('button', { name: 'Save profile' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  })
})
