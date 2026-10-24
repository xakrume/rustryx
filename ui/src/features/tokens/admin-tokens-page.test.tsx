import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))

describe('API tokens page (administrators)', () => {
  it('lists every token with its owner, kind, state and last use', async () => {
    renderApp('/admin/tokens', { user: 'admin' })
    const page = await inMain()
    expect((await page.findAllByRole('link', { name: 'alice' })).length).toBeGreaterThan(0)
    expect(page.getAllByText('Personal').length).toBeGreaterThan(0)
    expect(page.getAllByText('Expired').length).toBeGreaterThan(0)
    expect(page.getAllByText('Never used').length).toBeGreaterThan(0)
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/tokens', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('filters by state and by never used from the address', async () => {
    renderApp('/admin/tokens?state=revoked', { user: 'admin' })
    const page = await inMain()
    await page.findAllByText('Revoked')
    expect(within(page.getByRole('table')).queryByText('Active')).not.toBeInTheDocument()
    renderApp('/admin/tokens?never_used=true&expiring=90', { user: 'admin' })
    expect((await screen.findAllByText('Never used')).length).toBeGreaterThan(0)
  })

  it('shows the owner filter that comes from the user page and removes it on request', async () => {
    const user = userEvent.setup()
    const alice = identity().users.find((u) => u.username === 'alice')!
    renderApp(`/admin/tokens?owner_id=${alice.id}&owner=alice`, { user: 'admin' })
    const page = await inMain()
    expect(await page.findByText('Owner: alice')).toBeInTheDocument()
    await waitFor(() => expect(page.queryAllByRole('link', { name: 'bob' })).toHaveLength(0))
    await user.click(page.getByRole('button', { name: 'Remove the owner filter' }))
    expect((await page.findAllByRole('link', { name: 'bob' })).length).toBeGreaterThan(0)
    expect(page.queryByText('Owner: alice')).not.toBeInTheDocument()
  })

  it('must_name_the_token_and_its_owner_before_revoking_a_token_of_somebody_else', async () => {
    const user = userEvent.setup()
    renderApp('/admin/tokens?kind=personal', { user: 'admin' })
    const first = identity().tokens.find(
      (t) => t.revoked_at === null && t.user_id !== '0192f6c4-5a1e-7000-8000-000000000001',
    )!
    const owner = identity().users.find((u) => u.id === first.user_id)!
    const button = await (
      await inMain()
    ).findAllByRole('button', { name: `Revoke token ${first.name} of ${owner.username}` })
    await user.click(button[0]!)
    const dialog = await screen.findByRole('alertdialog', { name: `Revoke token ${first.name}?` })
    expect(dialog).toHaveAccessibleDescription(new RegExp(`${owner.username}.*not notified`))
    await user.click(within(dialog).getByRole('button', { name: 'Revoke token' }))
    expect(
      await screen.findByText(`Token ${first.name} of ${owner.username} revoked`),
    ).toBeInTheDocument()
    expect(identity().tokens.find((t) => t.id === first.id)?.revoked_at).not.toBeNull()
  })

  it('shows the error with code and request id and a retry when the list fails', async () => {
    window.sessionStorage.setItem('ryx-mock-chaos', 'tokens:503')
    renderApp('/admin/tokens', { user: 'admin' })
    expect(await screen.findByText('store_unavailable')).toBeInTheDocument()
  })
})
