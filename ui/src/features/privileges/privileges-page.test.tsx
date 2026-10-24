import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { commit, identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const PRIVILEGE = (name: string) => identity().privileges.find((p) => p.name === name)

describe('privileges page', () => {
  it('lists privileges with kind, repository, actions and the roles that use them', async () => {
    renderApp('/admin/privileges', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('button', { name: 'maven-releases-write' })).toBeInTheDocument()
    expect(page.getAllByText('Content selector').length).toBeGreaterThan(0)
    // A wildcard repository is marked with a warning badge, not only by its text.
    expect(page.getAllByText('All repositories').length).toBeGreaterThan(0)
    expect(page.getByText('internal-libs')).toBeInTheDocument()
    expect(
      page.getByRole('button', {
        name: 'Delete privilege nx-all (built-in privileges cannot be deleted)',
      }),
    ).toBeDisabled()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('filters by kind from the address', async () => {
    renderApp('/admin/privileges?kind=admin', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('button', { name: 'nx-all' })).toBeInTheDocument()
    expect(page.queryByRole('button', { name: 'npm-read' })).not.toBeInTheDocument()
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/privileges', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('opens a built-in privilege read only', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'View privilege nx-all' }))
    const dialog = await screen.findByRole('dialog', { name: /Privilege nx-all/ })
    expect(within(dialog).getByText(/built-in privilege/)).toBeInTheDocument()
    expect(within(dialog).getByText('Every action on everything')).toBeInTheDocument()
    expect(within(dialog).queryByRole('button', { name: 'Save privilege' })).not.toBeInTheDocument()
    await expectNoA11yViolations(dialog)
  })

  it('creates a repository view privilege and takes the format from the repository', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Create privilege' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'maven-snapshots-read')
    await within(dialog).findByRole('option', { name: /maven-snapshots \(maven2, hosted\)/ })
    await user.selectOptions(within(dialog).getByLabelText(/^Repository/), 'maven-snapshots')
    const format = within(dialog).getByLabelText('Format')
    expect(format).toHaveValue('maven2')
    expect(format).toBeDisabled()
    await user.click(within(dialog).getByRole('checkbox', { name: 'Write' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create privilege' }))
    expect(await screen.findByText('Privilege maven-snapshots-read created')).toBeInTheDocument()
    expect(PRIVILEGE('maven-snapshots-read')?.actions).toEqual(['browse', 'read', 'write'])
    expect(PRIVILEGE('maven-snapshots-read')?.grant).toEqual({
      type: 'repository-view',
      repository: 'maven-snapshots',
      format: 'maven2',
    })
  })

  it('warns about a wildcard repository, and louder when write is allowed', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    await user.selectOptions(within(dialog).getByLabelText(/^Repository/), '*')
    expect(within(dialog).getByText(/applies to every repository/)).toBeInTheDocument()
    expect(
      within(dialog).queryByText(/Write or delete on every repository/),
    ).not.toBeInTheDocument()
    await user.click(within(dialog).getByRole('checkbox', { name: 'Write' }))
    expect(within(dialog).getByText(/Write or delete on every repository/)).toBeInTheDocument()
    expect(within(dialog).getByLabelText('Format')).toBeEnabled()
  })

  it('must_not_save_a_privilege_without_an_action', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'nothing')
    await user.selectOptions(within(dialog).getByLabelText(/^Repository/), '*')
    await user.click(within(dialog).getByRole('checkbox', { name: 'Browse' }))
    await user.click(within(dialog).getByRole('checkbox', { name: 'Read' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create privilege' }))
    expect(
      await within(dialog).findByText(
        'Select at least one action: a privilege without actions is refused.',
      ),
    ).toBeInTheDocument()
    expect(PRIVILEGE('nothing')).toBeUndefined()
  })

  it('does not offer to create the admin grant and says why', async () => {
    renderApp('/admin/privileges?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    const radio = within(dialog).getByRole('radio', { name: 'Administration (built-in only)' })
    expect(radio).toBeDisabled()
    expect(radio).toHaveAccessibleDescription(/exists only as the built-in privilege/)
  })

  it('creates a content selector privilege and requires a selector', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    await user.click(
      within(dialog).getByRole('radio', { name: 'Part of a repository (content selector)' }),
    )
    await user.type(within(dialog).getByLabelText(/^Name/), 'docker-prod-read')
    await within(dialog).findByRole('option', { name: 'docker-prod' })
    await user.selectOptions(within(dialog).getByLabelText(/^Repository/), 'docker-releases')
    await user.click(within(dialog).getByRole('button', { name: 'Create privilege' }))
    expect(await within(dialog).findByText('Choose a content selector.')).toBeInTheDocument()
    await user.selectOptions(within(dialog).getByLabelText(/^Content selector/), 'docker-prod')
    await user.click(within(dialog).getByRole('button', { name: 'Create privilege' }))
    expect(await screen.findByText('Privilege docker-prod-read created')).toBeInTheDocument()
    expect(PRIVILEGE('docker-prod-read')?.grant.type).toBe('repository-content-selector')
  })

  it('maps a taken name from the server to the field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create privilege' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'npm-read')
    await user.selectOptions(within(dialog).getByLabelText(/^Repository/), '*')
    await user.click(within(dialog).getByRole('button', { name: 'Create privilege' }))
    expect(await within(dialog).findByText('A privilege with this name exists')).toBeInTheDocument()
  })

  it('must_show_the_conflict_dialog_when_the_privilege_changed_after_it_was_opened', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges?edit=0192f6c4-5a1e-7000-8000-00000000020a', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: /Edit privilege pypi-read/ })
    await user.type(within(dialog).getByLabelText('Description'), ' mine')
    commit({
      privileges: identity().privileges.map((p) =>
        p.name === 'pypi-read' ? { ...p, description: 'changed elsewhere' } : p,
      ),
    })
    await user.click(within(dialog).getByRole('button', { name: 'Save privilege' }))
    const conflict = await screen.findByRole('alertdialog', { name: 'Changed by someone else' })
    await user.click(within(conflict).getByRole('button', { name: 'Overwrite with my changes' }))
    expect(await screen.findByText('Privilege pypi-read saved')).toBeInTheDocument()
    expect(PRIVILEGE('pypi-read')?.description).toContain('mine')
  })

  it('names the roles that include a privilege before deleting it and detaches it', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete privilege npm-read' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete privilege\s+npm-read\s*\?/,
    })
    expect(dialog).toHaveAccessibleDescription(/npm-readers/)
    const confirm = within(dialog).getByRole('button', { name: 'Delete privilege' })
    expect(confirm).toBeDisabled()
    await user.type(within(dialog).getByRole('textbox', { name: 'Confirmation text' }), 'npm-read')
    await user.click(confirm)
    expect(await screen.findByText('Privilege npm-read deleted')).toBeInTheDocument()
    expect(PRIVILEGE('npm-read')).toBeUndefined()
    expect(identity().roles.find((r) => r.name === 'npm-readers')?.privilege_ids).toEqual([])
  })

  it('deletes an unused privilege after a plain confirmation and forces after a late 409', async () => {
    const user = userEvent.setup()
    renderApp('/admin/privileges', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete privilege helm-read' }),
    )
    // A role includes it by now: the delete answers 409, shows the blocker and the next try forces.
    commit({
      roles: identity().roles.map((r) =>
        r.name === 'npm-readers'
          ? { ...r, privilege_ids: [...r.privilege_ids, PRIVILEGE('helm-read')!.id] }
          : r,
      ),
    })
    const dialog = await screen.findByRole('alertdialog', { name: /Delete privilege\s+helm-read/ })
    await user.click(within(dialog).getByRole('button', { name: 'Delete privilege' }))
    expect(await within(dialog).findByText('resource_in_use')).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Delete privilege' }))
    await waitFor(() => expect(PRIVILEGE('helm-read')).toBeUndefined())
  })
})
