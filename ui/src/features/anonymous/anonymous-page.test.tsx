import type { BoundFunctions, queries } from '@testing-library/dom'
import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { commit, identity, resetIdentity } from '@/features/roles/mock-store'
import { protectRepositories } from '@/features/roles/test-support'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

let restore: () => void
beforeEach(() => {
  resetIdentity()
  restore = protectRepositories()
})
afterEach(() => restore())

type Page = BoundFunctions<typeof queries>
const inMain = async (): Promise<Page> => within(await screen.findByRole('main'))
const FLAG = (name: string) => repositories.find((r) => r.name === name)?.allow_anonymous
const flagSwitch = (page: Page, name: string): Promise<HTMLElement> =>
  page.findByRole('switch', { name: `Allow anonymous access to ${name}` })

describe('anonymous access page', () => {
  it('shows the global switch as read only with the reason, and the counts', async () => {
    renderApp('/admin/anonymous', { user: 'admin' })
    const page = await inMain()
    const global = await page.findByRole('switch', { name: 'Allow anonymous access' })
    expect(global).toBeDisabled()
    expect(global).toBeChecked()
    expect(page.getByText('This switch is read only in this version')).toBeInTheDocument()
    expect(page.getByText(/auth\.anonymous_enabled/)).toBeInTheDocument()
    expect(page.getByText(/repositories have their flag on/)).toBeInTheDocument()
    expect(page.getByText('nx-anonymous')).toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/anonymous', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('lists every repository with its flag and the effective result, and filters by flag', async () => {
    renderApp('/admin/anonymous?allow_anonymous=false', { user: 'admin' })
    const page = await inMain()
    expect(await flagSwitch(page, 'docker-releases')).not.toBeChecked()
    expect(
      page.queryByRole('switch', { name: 'Allow anonymous access to docker-group' }),
    ).not.toBeInTheDocument()
    expect(page.getAllByText('Private').length).toBeGreaterThan(0)
  })

  it('turns a flag off at once, without asking', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous', { user: 'admin' })
    await user.click(await flagSwitch(await inMain(), 'docker-group'))
    expect(
      await screen.findByText('Anonymous access removed from 1 repository'),
    ).toBeInTheDocument()
    expect(FLAG('docker-group')).toBe(false)
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
  })

  it('must_ask_before_making_a_repository_public_and_change_nothing_when_declined', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous', { user: 'admin' })
    await user.click(await flagSwitch(await inMain(), 'docker-releases'))
    const dialog = await screen.findByRole('alertdialog', { name: 'Make repositories public?' })
    expect(dialog).toHaveTextContent('docker-releases')
    expect(dialog).toHaveAccessibleDescription(/without an account/)
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    expect(FLAG('docker-releases')).toBe(false)
    await user.click(await flagSwitch(await inMain(), 'docker-releases'))
    await user.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Make public' }),
    )
    expect(await screen.findByText('Anonymous access allowed for 1 repository')).toBeInTheDocument()
    expect(FLAG('docker-releases')).toBe(true)
    await waitFor(async () =>
      expect(await flagSwitch(await inMain(), 'docker-releases')).toBeChecked(),
    )
  })

  it('changes several repositories in bulk, naming them when they become public', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous?allow_anonymous=false&q=docker', { user: 'admin' })
    const page = await inMain()
    await user.click(await page.findByRole('checkbox', { name: 'Select docker-releases' }))
    await user.click(page.getByRole('checkbox', { name: 'Select docker-snapshots' }))
    const bar = page.getByRole('region', { name: 'Bulk actions' })
    await user.click(within(bar).getByRole('button', { name: 'Allow anonymous' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Make repositories public?' })
    expect(dialog).toHaveTextContent('docker-releases, docker-snapshots')
    await user.click(within(dialog).getByRole('button', { name: 'Make public' }))
    expect(
      await screen.findByText('Anonymous access allowed for 2 repositories'),
    ).toBeInTheDocument()
    expect(FLAG('docker-snapshots')).toBe(true)
  })

  it('removes the flag of selected repositories in bulk without a question', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous?allow_anonymous=true&q=helm', { user: 'admin' })
    const page = await inMain()
    await page.findByRole('checkbox', { name: 'Select helm-group' })
    await user.click(page.getByRole('checkbox', { name: 'Select all loaded rows' }))
    await user.click(
      within(page.getByRole('region', { name: 'Bulk actions' })).getByRole('button', {
        name: 'Disallow anonymous',
      }),
    )
    expect(
      await screen.findByText(/Anonymous access removed from \d+ repositories/),
    ).toBeInTheDocument()
    expect(FLAG('helm-group')).toBe(false)
  })

  it('shortens a long list of names in the confirmation', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous?allow_anonymous=false', { user: 'admin' })
    const page = await inMain()
    await page.findByRole('checkbox', { name: 'Select docker-releases' })
    await user.click(page.getByRole('checkbox', { name: 'Select all loaded rows' }))
    await user.click(
      within(page.getByRole('region', { name: 'Bulk actions' })).getByRole('button', {
        name: 'Allow anonymous',
      }),
    )
    const dialog = await screen.findByRole('alertdialog', { name: 'Make repositories public?' })
    expect(dialog).toHaveTextContent(/and \d+ more/)
  })

  it('shows the error with code and keeps the dialog when the change fails', async () => {
    const user = userEvent.setup()
    renderApp('/admin/anonymous', {
      user: 'admin',
      overrides: [
        http.put('/api/v1/anonymous-access/repositories', () =>
          HttpResponse.json(
            { error: { code: 'store_unavailable', message: 'down' } },
            { status: 503 },
          ),
        ),
      ],
    })
    await user.click(await flagSwitch(await inMain(), 'docker-releases'))
    const dialog = await screen.findByRole('alertdialog', { name: 'Make repositories public?' })
    await user.click(within(dialog).getByRole('button', { name: 'Make public' }))
    expect(await within(dialog).findByText('store_unavailable')).toBeInTheDocument()
    expect(FLAG('docker-releases')).toBe(false)
  })

  it('marks a repository whose flag is on while the global switch is off', async () => {
    commit({ anonymous: { ...identity().anonymous, globalEnabled: false } })
    for (const [i, repo] of repositories.entries())
      repositories[i] = { ...repo, anonymous_effective: false }
    renderApp('/admin/anonymous?allow_anonymous=true', { user: 'admin' })
    const page = await inMain()
    expect(
      (await page.findAllByText('Private: the flag is on but the global switch is off')).length,
    ).toBeGreaterThan(0)
    expect(page.getByText(/every repository is private to anonymous callers/)).toBeInTheDocument()
  })

  it('turns the global switch off and on when the server makes it editable, asking each time', async () => {
    const user = userEvent.setup()
    commit({ anonymous: { ...identity().anonymous, editable: true } })
    renderApp('/admin/anonymous', { user: 'admin' })
    const global = await (await inMain()).findByRole('switch', { name: 'Allow anonymous access' })
    expect(global).toBeEnabled()
    await user.click(global)
    await user.click(
      within(
        await screen.findByRole('alertdialog', { name: 'Turn anonymous access off?' }),
      ).getByRole('button', { name: 'Turn it off' }),
    )
    expect(await screen.findByText('Anonymous access is now Off')).toBeInTheDocument()
    expect(identity().anonymous.globalEnabled).toBe(false)
    await waitFor(async () =>
      expect(
        await (await inMain()).findByRole('switch', { name: 'Allow anonymous access' }),
      ).not.toBeChecked(),
    )
    await user.click(
      await (await inMain()).findByRole('switch', { name: 'Allow anonymous access' }),
    )
    const enable = await screen.findByRole('alertdialog', { name: 'Allow anonymous access?' })
    expect(enable).toHaveAccessibleDescription(/without an account/)
    await user.click(within(enable).getByRole('button', { name: 'Allow anonymous access' }))
    expect(await screen.findByText('Anonymous access is now On')).toBeInTheDocument()
  })

  it('must_show_the_conflict_dialog_when_the_global_switch_changed_elsewhere', async () => {
    const user = userEvent.setup()
    commit({ anonymous: { ...identity().anonymous, editable: true } })
    renderApp('/admin/anonymous', { user: 'admin' })
    const global = await (await inMain()).findByRole('switch', { name: 'Allow anonymous access' })
    await user.click(global)
    const confirm = await screen.findByRole('alertdialog', { name: 'Turn anonymous access off?' })
    // Another administrator flips it first, so the ETag in this page is stale.
    commit({ anonymous: { ...identity().anonymous, globalEnabled: false } })
    await user.click(within(confirm).getByRole('button', { name: 'Turn it off' }))
    expect(
      await screen.findByRole('alertdialog', { name: 'Changed by someone else' }),
    ).toBeInTheDocument()
  })

  it('shows the error panel with the code when the state cannot be read', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'anonymous:503')
    renderApp('/admin/anonymous', { user: 'admin' })
    expect((await screen.findAllByText('store_unavailable')).length).toBeGreaterThan(0)
  })
})
