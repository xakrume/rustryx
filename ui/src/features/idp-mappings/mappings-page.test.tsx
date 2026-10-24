import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const MAPPING = (group: string) => identity().mappings.find((m) => m.external_group === group)

describe('group mappings page', () => {
  it('lists the OIDC mappings in evaluation order and states where administrators come from', async () => {
    renderApp('/admin/group-mappings', { user: 'admin' })
    const page = await inMain()
    expect(
      await page.findByRole('tab', { name: 'OIDC groups', selected: true }),
    ).toBeInTheDocument()
    await page.findByText('/rustryx/frontend/readers')
    const rows = page.getAllByRole('row')
    expect(rows[1]).toHaveTextContent('50')
    expect(rows[1]).toHaveTextContent('/rustryx/team-payments/publishers')
    expect(page.getByText('Administrators come only from the configuration')).toBeInTheDocument()
    expect(await page.findByText('/rustryx/admins')).toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('switches to the LDAP tab, which has its own mappings and admin groups', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/admin/group-mappings', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('tab', { name: 'LDAP groups' }))
    const page = await inMain()
    expect(await page.findByText('cn=platform,ou=groups,dc=example,dc=org')).toBeInTheDocument()
    expect(page.queryByText('/rustryx/security')).not.toBeInTheDocument()
    expect(page.getByText('No admin groups are configured for this provider.')).toBeInTheDocument()
    expect(router.state.location.search).toMatchObject({ provider: 'ldap' })
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/group-mappings', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('warns that matching is exact and case sensitive when adding a mapping', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    await user.click(await (await inMain()).findByRole('button', { name: 'Add mapping' }))
    const dialog = await screen.findByRole('dialog', { name: 'Add OIDC groups group mapping' })
    expect(within(dialog).getByText('Matching is exact and case sensitive')).toBeInTheDocument()
    expect(
      within(dialog).getByText(
        /Nothing is trimmed, folded to lower case or matched with wildcards/,
      ),
    ).toBeInTheDocument()
    await expectNoA11yViolations(dialog)
  })

  it('must_not_offer_nx_admin_or_nx_anonymous_as_the_role_of_a_mapping', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog')
    await within(dialog).findByRole('option', { name: 'nx-developer' })
    const names = within(within(dialog).getByLabelText(/^Role/))
      .getAllByRole('option')
      .map((o) => o.textContent)
    expect(names).not.toContain('nx-admin')
    expect(names).not.toContain('nx-anonymous')
    expect(
      within(dialog).getByText(/administrators come only from the configured admin groups/),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Add mapping' }))
    expect(
      await within(dialog).findByText('Enter the group exactly as the provider reports it.'),
    ).toBeInTheDocument()
    expect(within(dialog).getByText('Choose a role.')).toBeInTheDocument()
  })

  it('must_send_the_group_exactly_as_typed_without_trimming_or_changing_its_case', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog')
    await user.type(within(dialog).getByLabelText(/^Group/), '/Rustryx/New Team ')
    expect(within(dialog).getByText(/starts or ends with a space/)).toBeInTheDocument()
    await within(dialog).findByRole('option', { name: 'npm-readers' })
    await user.selectOptions(within(dialog).getByLabelText(/^Role/), 'npm-readers')
    await user.clear(within(dialog).getByLabelText(/^Priority/))
    await user.type(within(dialog).getByLabelText(/^Priority/), '70')
    await user.click(within(dialog).getByRole('button', { name: 'Add mapping' }))
    expect(await screen.findByText(/Mapping for \/Rustryx\/New Team\s+added/)).toBeInTheDocument()
    expect(MAPPING('/Rustryx/New Team ')).toMatchObject({ provider: 'oidc', priority: 70 })
    expect(MAPPING('/rustryx/new team')).toBeUndefined()
  })

  it('warns about values that will probably match nothing, without blocking', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog')
    await user.type(within(dialog).getByLabelText(/^Group/), 'rustryx/team')
    expect(within(dialog).getByText(/Keycloak group paths start with \//)).toBeInTheDocument()
    expect(within(dialog).getByText(/cannot check that the group exists/)).toBeInTheDocument()
  })

  it('warns when an LDAP value does not look like a distinguished name', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings?provider=ldap&new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Add LDAP groups group mapping' })
    await user.type(within(dialog).getByLabelText(/^Group/), 'platform')
    expect(within(dialog).getByText(/has no = sign/)).toBeInTheDocument()
  })

  it('maps a duplicate group from the server to the field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog')
    await user.type(within(dialog).getByLabelText(/^Group/), '/rustryx/security')
    await within(dialog).findByRole('option', { name: 'auditors' })
    await user.selectOptions(within(dialog).getByLabelText(/^Role/), 'auditors')
    await user.click(within(dialog).getByRole('button', { name: 'Add mapping' }))
    expect(await within(dialog).findByText(/already maps this exact group/)).toBeInTheDocument()
    expect(within(dialog).getByLabelText(/^Group/)).toHaveFocus()
  })

  it('edits the role and priority of a mapping and keeps the group read only', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Edit mapping of /rustryx/security' }),
    )
    const dialog = await screen.findByRole('dialog', { name: 'Edit mapping of /rustryx/security' })
    expect(within(dialog).getByLabelText(/^Group/)).toHaveAttribute('readonly')
    await user.clear(within(dialog).getByLabelText(/^Priority/))
    await user.type(within(dialog).getByLabelText(/^Priority/), '5')
    await user.click(within(dialog).getByRole('button', { name: 'Save mapping' }))
    expect(await screen.findByText('Mapping for /rustryx/security saved')).toBeInTheDocument()
    expect(MAPPING('/rustryx/security')?.priority).toBe(5)
  })

  it('shows the error when the version of a mapping cannot be read, and changes nothing', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/idp-group-mappings/:id', () =>
          HttpResponse.json(
            { error: { code: 'route_not_found', message: 'No such route' } },
            { status: 404 },
          ),
        ),
      ],
    })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Edit mapping of /rustryx/security' }),
    )
    const dialog = await screen.findByRole('dialog', { name: 'Edit mapping of /rustryx/security' })
    await user.clear(within(dialog).getByLabelText(/^Priority/))
    await user.type(within(dialog).getByLabelText(/^Priority/), '5')
    await user.click(within(dialog).getByRole('button', { name: 'Save mapping' }))
    expect(await screen.findByText('route_not_found')).toBeInTheDocument()
    expect(MAPPING('/rustryx/security')?.priority).toBe(200)
  })

  it('moves a mapping up and down from the keyboard and announces the new position', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    const page = await inMain()
    const firstUp = await page.findAllByRole('button', {
      name: 'Move /rustryx/team-payments/publishers up',
    })
    expect(firstUp[0]).toBeDisabled()
    await user.click(page.getByRole('button', { name: 'Move /rustryx/security up' }))
    await waitFor(() => expect(MAPPING('/rustryx/security')?.priority).toBe(100))
    await waitFor(() =>
      expect(
        identity()
          .mappings.filter((m) => m.external_group === '/rustryx/team-payments/publishers')
          .map((m) => m.priority)
          .sort((a, b) => a - b),
      ).toEqual([50, 200]),
    )
    expect(await page.findByText('Moved /rustryx/security to position 3 of 4')).toBeInTheDocument()
    expect(page.getByRole('button', { name: 'Move /rustryx/security up' })).toBeEnabled()
  })

  it('steps past a neighbour that has the same priority', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    const page = await inMain()
    await user.click(
      await page.findByRole('button', { name: 'Move /rustryx/frontend/readers down' }),
    )
    await waitFor(() => expect(MAPPING('/rustryx/frontend/readers')?.priority).toBe(101))
  })

  it('deletes a mapping after a confirmation that names the group and the role', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete mapping of /rustryx/security' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete group mapping\s+\/rustryx\/security\s*\?/,
    })
    expect(dialog).toHaveAccessibleDescription(/auditors/)
    await user.click(within(dialog).getByRole('button', { name: 'Delete mapping' }))
    expect(await screen.findByText('Mapping for /rustryx/security deleted')).toBeInTheDocument()
    expect(MAPPING('/rustryx/security')).toBeUndefined()
  })

  it('resolves a group list exactly and tells what each role came from', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    const page = await inMain()
    const box = await page.findByLabelText('Groups')
    await user.click(page.getByRole('button', { name: 'Resolve roles' }))
    expect(await page.findByText('Enter at least one group.')).toBeInTheDocument()
    await user.click(box)
    await user.paste('/rustryx/admins\n/rustryx/team-payments/publishers\n/Rustryx/Security')
    await user.click(page.getByRole('button', { name: 'Resolve roles' }))
    expect(await page.findByText('Admin group')).toBeInTheDocument()
    expect(page.getAllByText('Mapping')).toHaveLength(2)
    expect(page.getByText('/Rustryx/Security')).toBeInTheDocument()
    expect(page.getByText('The sign in would be accepted.')).toBeInTheDocument()
  })

  it('says the sign in would be refused when nothing matches', async () => {
    const user = userEvent.setup()
    renderApp('/admin/group-mappings', { user: 'admin' })
    const page = await inMain()
    await user.click(await page.findByLabelText('Groups'))
    await user.paste('/nothing/here')
    await user.click(page.getByRole('button', { name: 'Resolve roles' }))
    expect(await page.findByText(/The sign in would be refused/)).toBeInTheDocument()
    expect(page.getByText('No role results.')).toBeInTheDocument()
  })

  it('still explains the rule when the admin groups cannot be read', async () => {
    renderApp('/admin/group-mappings', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/auth/providers/details', () =>
          HttpResponse.json(
            { error: { code: 'store_unavailable', message: 'down' } },
            { status: 503 },
          ),
        ),
      ],
    })
    const page = await inMain()
    expect(await page.findByText('The admin groups could not be read.')).toBeInTheDocument()
    expect(page.getByText(/never granted by a mapping/)).toBeInTheDocument()
  })
})
