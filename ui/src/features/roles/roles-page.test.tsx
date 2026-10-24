import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { commit, identity, resetIdentity } from './mock-store'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const main = () => within(screen.getByRole('main'))
const inMain = async () => within(await screen.findByRole('main'))

describe('roles page', () => {
  it('lists roles with built-in markers and usage, without accessibility violations', async () => {
    renderApp('/admin/roles', { user: 'admin' })
    expect(
      await (await inMain()).findByRole('button', { name: 'maven-publishers' }),
    ).toBeInTheDocument()
    expect((await inMain()).getAllByText('Built-in').length).toBeGreaterThanOrEqual(3)
    expect(
      (await inMain()).getByRole('button', {
        name: 'Delete role nx-admin (built-in roles cannot be deleted)',
      }),
    ).toBeDisabled()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('filters by name from the address and says so when nothing matches', async () => {
    renderApp('/admin/roles?q=npm', { user: 'admin' })
    expect(await (await inMain()).findByRole('button', { name: 'npm-readers' })).toBeInTheDocument()
    expect(
      (await inMain()).queryByRole('button', { name: 'maven-publishers' }),
    ).not.toBeInTheDocument()
    renderApp('/admin/roles?q=zzzz', { user: 'admin' })
    expect(await screen.findAllByText('No results for these filters')).not.toHaveLength(0)
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/roles', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Create role' })).not.toBeInTheDocument()
  })

  it('shows the error code, the request id and a countdown when the server is sick', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'roles:503')
    renderApp('/admin/roles', { user: 'admin' })
    expect(await screen.findByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Retry in \d+ s/ })).toBeDisabled()
  })

  it('shows the forbidden panel when the API answers 403 for the list', async () => {
    renderApp('/admin/roles', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/roles', () =>
          HttpResponse.json({ error: { code: 'forbidden', message: 'No' } }, { status: 403 }),
        ),
      ],
    })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('opens a built-in role read only, with its effective privileges', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'View role nx-admin' }))
    const dialog = await screen.findByRole('dialog', { name: /Role nx-admin/ })
    expect(within(dialog).getByText(/built-in role/)).toBeInTheDocument()
    expect(within(dialog).queryByRole('button', { name: 'Save role' })).not.toBeInTheDocument()
    expect(await within(dialog).findAllByText('nx-all')).not.toHaveLength(0)
    await expectNoA11yViolations(dialog)
  })

  it('creates a role with a privilege and lists it', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Create role' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create role' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'qa-team')
    await user.click(await within(dialog).findByRole('checkbox', { name: 'npm-read' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create role' }))
    expect(await screen.findByText('Role qa-team created')).toBeInTheDocument()
    expect(await (await inMain()).findByRole('button', { name: 'qa-team' })).toBeInTheDocument()
    expect(identity().roles.find((r) => r.name === 'qa-team')?.privilege_ids).toHaveLength(1)
  })

  it('validates the name next to the field and maps a taken name from the server', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Create role' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create role' })
    await user.click(within(dialog).getByRole('button', { name: 'Create role' }))
    expect(
      await within(dialog).findByText('Enter a name of 1 to 128 characters.'),
    ).toBeInTheDocument()
    await user.type(within(dialog).getByLabelText(/^Name/), 'npm-readers')
    await user.click(within(dialog).getByRole('button', { name: 'Create role' }))
    expect(await within(dialog).findByText('A role with this name exists')).toBeInTheDocument()
  })

  it('edits a role and saves it with the ETag it read', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-00000000000e', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit role npm-readers/ })
    const description = within(dialog).getByLabelText('Description')
    await user.clear(description)
    await user.type(description, 'Read all npm')
    await user.click(within(dialog).getByRole('button', { name: 'Save role' }))
    expect(await screen.findByText('Role npm-readers saved')).toBeInTheDocument()
    expect(identity().roles.find((r) => r.name === 'npm-readers')?.description).toBe('Read all npm')
    // The save leaves the form clean, so closing it must not ask to discard anything.
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
  })

  it('must_ask_before_discarding_unsaved_edits_and_keep_them_when_told_to_stay', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-00000000000e', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit role npm-readers/ })
    await user.type(within(dialog).getByLabelText('Description'), ' more')
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    const guard = await screen.findByRole('alertdialog', { name: 'Discard unsaved changes?' })
    await user.click(within(guard).getByRole('button', { name: 'Keep editing' }))
    expect(within(dialog).getByLabelText('Description')).toHaveValue('Read npm packages more')
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await user.click(await screen.findByRole('button', { name: 'Discard and leave' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(identity().roles.find((r) => r.name === 'npm-readers')?.description).toBe(
      'Read npm packages',
    )
  })

  it('must_show_the_conflict_dialog_when_the_role_changed_after_it_was_opened', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-000000000011', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit role ci-bots/ })
    await user.type(within(dialog).getByLabelText('Description'), ' (mine)')
    // Somebody else saves first: the ETag the form holds is stale now.
    commit({
      roles: identity().roles.map((r) =>
        r.name === 'ci-bots' ? { ...r, description: 'changed elsewhere' } : r,
      ),
    })
    await user.click(within(dialog).getByRole('button', { name: 'Save role' }))
    const conflict = await screen.findByRole('alertdialog', { name: 'Changed by someone else' })
    expect(conflict).toHaveTextContent('role ci-bots')
    await user.click(within(conflict).getByRole('button', { name: 'Overwrite with my changes' }))
    expect(await screen.findByText('Role ci-bots saved')).toBeInTheDocument()
    expect(identity().roles.find((r) => r.name === 'ci-bots')?.description).toContain('(mine)')
  })

  it('reloads the latest version from the conflict dialog and keeps the dialog open', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-000000000011', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit role ci-bots/ })
    await user.type(within(dialog).getByLabelText('Description'), ' (mine)')
    commit({
      roles: identity().roles.map((r) =>
        r.name === 'ci-bots' ? { ...r, description: 'changed elsewhere' } : r,
      ),
    })
    await user.click(within(dialog).getByRole('button', { name: 'Save role' }))
    const conflict = await screen.findByRole('alertdialog', { name: 'Changed by someone else' })
    await user.click(within(conflict).getByRole('button', { name: 'Reload the latest version' }))
    await waitFor(() =>
      expect(within(dialog).getByLabelText('Description')).toHaveValue('changed elsewhere'),
    )
  })

  it('must_not_offer_a_nested_role_that_would_form_a_cycle', async () => {
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-00000000000c', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit role maven-publishers/ })
    const box = await within(dialog).findByRole('checkbox', { name: /release-managers/ })
    expect(box).toBeDisabled()
    expect(
      within(dialog).getByText('Would create a cycle: release-managers includes maven-publishers'),
    ).toBeInTheDocument()
  })

  it('shows a cycle the server finds at the nested roles field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles?edit=0192f6c4-5a1e-7000-8000-00000000000e', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/roles/:id', () =>
          HttpResponse.json({ error: { code: 'role_cycle', message: 'cycle' } }, { status: 409 }),
        ),
      ],
    })
    const dialog = await screen.findByRole('dialog', { name: /Edit role npm-readers/ })
    await user.click(within(dialog).getByRole('button', { name: 'Save role' }))
    expect(await within(dialog).findByText(/would form a cycle/)).toBeInTheDocument()
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
  })

  it('must_ask_for_confirmation_naming_the_role_and_what_uses_it_before_deleting_a_role_in_use', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete role maven-publishers' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete role\s+maven-publishers\s*\?/,
    })
    expect(dialog).toHaveAccessibleDescription(/in use/)
    expect(within(dialog).getByText(/users? holds? it directly/)).toBeInTheDocument()
    const confirm = within(dialog).getByRole('button', { name: 'Delete role' })
    expect(confirm).toBeDisabled()
    expect(identity().roles.some((r) => r.name === 'maven-publishers')).toBe(true)
    await user.type(
      within(dialog).getByRole('textbox', { name: 'Confirmation text' }),
      'maven-publishers',
    )
    await user.click(confirm)
    expect(await screen.findByText('Role maven-publishers deleted')).toBeInTheDocument()
    expect(identity().roles.some((r) => r.name === 'maven-publishers')).toBe(false)
    await waitFor(() =>
      expect(main().queryByRole('button', { name: 'maven-publishers' })).not.toBeInTheDocument(),
    )
  })

  it('deletes an unused role after a plain confirmation', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete role release-managers' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete role\s+release-managers/,
    })
    expect(within(dialog).queryByRole('textbox')).not.toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Delete role' }))
    expect(await screen.findByText('Role release-managers deleted')).toBeInTheDocument()
  })

  it('forces the delete on the next try when a role became used after the list was read', async () => {
    const user = userEvent.setup()
    renderApp('/admin/roles', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete role release-managers' }),
    )
    // Another administrator gives the role to a user after the list was loaded.
    commit({
      users: identity().users.map((u) =>
        u.username === 'bob'
          ? { ...u, manualRoleIds: [...u.manualRoleIds, '0192f6c4-5a1e-7000-8000-000000000012'] }
          : u,
      ),
    })
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete role\s+release-managers/,
    })
    await user.click(within(dialog).getByRole('button', { name: 'Delete role' }))
    expect(await within(dialog).findByText('resource_in_use')).toBeInTheDocument()
    expect(within(dialog).getByText(/1 user/)).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Delete role' }))
    expect(await screen.findByText('Role release-managers deleted')).toBeInTheDocument()
  })
})
