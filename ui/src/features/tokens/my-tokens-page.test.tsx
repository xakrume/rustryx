import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const ADMIN_ID = '0192f6c4-5a1e-7000-8000-000000000001'

async function createToken(
  user: ReturnType<typeof userEvent.setup>,
  name: string,
): Promise<HTMLElement> {
  await user.click(await (await inMain()).findByRole('button', { name: 'Create token' }))
  const dialog = await screen.findByRole('dialog', { name: 'Create token' })
  await user.type(within(dialog).getByLabelText(/^Name/), name)
  return dialog
}

describe('personal tokens page', () => {
  it('lists the own tokens with state, expiry and last use, and a placeholder in the usage hint', async () => {
    renderApp('/me/tokens', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByText('ci-publisher')).toBeInTheDocument()
    expect(page.getAllByText('Active').length).toBeGreaterThan(0)
    expect(page.getByText(/curl -H "Authorization: Bearer <TOKEN>"/)).toBeInTheDocument()
    expect(page.getByText(/New tokens expire after 90 days/)).toBeInTheDocument()
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('must_show_the_secret_once_and_drop_it_after_the_user_stored_it', async () => {
    const user = userEvent.setup()
    const { queryClient } = renderApp('/me/tokens', { user: 'admin' })
    const form = await createToken(user, 'ci-bot')
    await user.click(within(form).getByRole('button', { name: 'Create token' }))
    const secretDialog = await screen.findByRole('dialog', { name: 'Copy your API token now' })
    const secret = /ryx_[A-Za-z0-9]{43}/.exec(secretDialog.textContent ?? '')?.[0]
    expect(secret).toBeDefined()
    // Cannot be closed before the user confirmed that it was stored.
    const done = within(secretDialog).getByRole('button', { name: 'Done' })
    expect(done).toBeDisabled()
    await user.keyboard('{Escape}')
    expect(screen.getByRole('dialog', { name: 'Copy your API token now' })).toBeInTheDocument()
    await expectNoA11yViolations(secretDialog)
    await user.click(
      within(secretDialog).getByRole('checkbox', { name: 'I stored it in a safe place' }),
    )
    await user.click(done)
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    // Gone from the page, from every cache, and the list shows only the prefix.
    expect(document.body.textContent).not.toContain(secret)
    expect(
      JSON.stringify(
        queryClient
          .getQueryCache()
          .getAll()
          .map((q) => q.state.data),
      ),
    ).not.toContain(secret!)
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((m) => m.state.data),
      ),
    ).not.toContain(secret!)
    const page = await inMain()
    expect(await page.findByText('ci-bot')).toBeInTheDocument()
    expect(page.getByText(secret!.slice(0, 8))).toBeInTheDocument()
  })

  it('offers the lifetimes the policy allows and marks the default', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens', { user: 'admin' })
    const dialog = await createToken(user, 'x')
    const expiry = within(dialog).getByLabelText('Expires after')
    expect(expiry).toHaveValue('90')
    expect(within(expiry).getByRole('option', { name: '90 days (default)' })).toBeInTheDocument()
    expect(
      within(expiry)
        .getAllByRole('option')
        .map((o) => o.getAttribute('value')),
    ).toEqual(['7', '30', '60', '90', '180', '365'])
    expect(
      within(dialog).getByText('The longest allowed lifetime is 365 days.'),
    ).toBeInTheDocument()
    await user.selectOptions(expiry, '30')
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    await screen.findByRole('dialog', { name: 'Copy your API token now' })
    const created = identity().tokens.find((t) => t.name === 'x')!
    const days = (Date.parse(created.expires_at!) - Date.parse(created.created_at)) / 86_400_000
    expect(Math.round(days)).toBe(30)
  })

  it('builds custom scopes and keeps read and browse selected while write is', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens', { user: 'admin' })
    const dialog = await createToken(user, 'publisher')
    await user.click(within(dialog).getByRole('radio', { name: 'Custom' }))
    await within(dialog).findByRole('option', { name: 'maven-releases' })
    await user.selectOptions(within(dialog).getByLabelText('Repository'), 'maven-releases')
    await user.click(within(dialog).getByRole('checkbox', { name: 'Write' }))
    expect(within(dialog).getByRole('checkbox', { name: 'Read' })).toBeChecked()
    expect(within(dialog).getByRole('checkbox', { name: 'Read' })).toBeDisabled()
    expect(within(dialog).getByRole('checkbox', { name: 'Browse' })).toBeDisabled()
    expect(within(dialog).getByText(/Write includes browse and read/)).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    await screen.findByRole('dialog', { name: 'Copy your API token now' })
    expect(identity().tokens.find((t) => t.name === 'publisher')?.scopes).toEqual([
      'repo:maven-releases:browse',
      'repo:maven-releases:read',
      'repo:maven-releases:write',
    ])
  })

  it('sends the read only preset', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens', { user: 'admin' })
    const dialog = await createToken(user, 'reader')
    await user.click(within(dialog).getByRole('radio', { name: 'Read only' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    await screen.findByRole('dialog', { name: 'Copy your API token now' })
    expect(identity().tokens.find((t) => t.name === 'reader')?.scopes).toEqual([
      'repo:*:browse',
      'repo:*:read',
    ])
  })

  it('refuses custom scopes without a repository and a name that is empty', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    expect(
      await within(dialog).findByText('Enter a name of 1 to 128 characters.'),
    ).toBeInTheDocument()
    await user.type(within(dialog).getByLabelText(/^Name/), 'x')
    await user.click(within(dialog).getByRole('radio', { name: 'Custom' }))
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    expect(
      await within(dialog).findByText('Choose at least one repository with one action.'),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Add repository' }))
    expect(within(dialog).getByRole('button', { name: 'Remove scope 2' })).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Remove scope 2' }))
    expect(identity().tokens.some((t) => t.name === 'x')).toBe(false)
  })

  it('shows code and request id when the server throttles the creation', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/me/tokens', () =>
          HttpResponse.json(
            {
              error: { code: 'rate_limited', message: 'Too many requests', request_id: 'req-429' },
            },
            { status: 429, headers: { 'Retry-After': '5', 'X-Request-Id': 'req-429' } },
          ),
        ),
      ],
    })
    const dialog = await createToken(user, 'later')
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    expect(await screen.findByText('rate_limited')).toBeInTheDocument()
    expect(screen.getByText('req-429')).toBeInTheDocument()
    expect(
      screen.queryByRole('dialog', { name: 'Copy your API token now' }),
    ).not.toBeInTheDocument()
  })

  it('must_ask_for_confirmation_naming_the_token_before_revoking_it', async () => {
    const user = userEvent.setup()
    renderApp('/me/tokens', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Revoke token ci-publisher' }),
    )
    const dialog = await screen.findByRole('alertdialog', { name: 'Revoke token ci-publisher?' })
    expect(dialog).toHaveAccessibleDescription(/cannot be restored/)
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toHaveFocus()
    expect(
      identity().tokens.find((t) => t.name === 'ci-publisher' && t.user_id === ADMIN_ID)
        ?.revoked_at,
    ).toBeNull()
    await user.click(within(dialog).getByRole('button', { name: 'Revoke token' }))
    expect(await screen.findByText('Token ci-publisher revoked')).toBeInTheDocument()
    expect(
      identity().tokens.find((t) => t.name === 'ci-publisher' && t.user_id === ADMIN_ID)
        ?.revoked_at,
    ).not.toBeNull()
    await waitFor(async () =>
      expect((await inMain()).getAllByText('Revoked').length).toBeGreaterThan(0),
    )
  })

  it('filters by state from the address', async () => {
    renderApp('/me/tokens?state=expired', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByText('docs-build')).toBeInTheDocument()
    expect(within(page.getByRole('table')).queryByText('Active')).not.toBeInTheDocument()
  })

  it('invites to create the first token when there is none', async () => {
    renderApp('/me/tokens', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/me/tokens', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('You have no tokens yet')).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Create token' }).length).toBe(2)
  })

  it('works for an account that signs in through the identity provider', async () => {
    renderApp('/me/tokens', { user: 'alice' })
    expect(
      await (await inMain()).findByRole('button', { name: 'Create token' }),
    ).toBeInTheDocument()
  })
})
