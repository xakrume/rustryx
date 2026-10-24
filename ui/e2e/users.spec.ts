import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  signIn,
  test,
} from './identity-support'

const ALICE = '/admin/users/0192f6c4-5a1e-7000-8000-000000000005'

test.describe('users (keyboard only)', () => {
  test('opens a user from the list, edits the manual roles and disables the account after a confirmation', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/users')
    const main = page.getByRole('main')
    await activate(page, main.getByRole('link', { name: 'bob' }))
    await expect(page.getByRole('heading', { level: 1, name: 'bob' })).toBeFocused()

    await activate(page, main.getByRole('button', { name: 'Edit manual roles' }))
    const roles = page.getByRole('dialog', { name: 'Manual roles of bob' })
    await activate(page, roles.getByRole('checkbox', { name: 'ci-bots' }), 'Space')
    await activate(page, roles.getByRole('button', { name: 'Save roles' }))
    await expect(page.getByText('Roles of bob saved').first()).toBeVisible()
    await expect(main.getByText('ci-bots')).toBeVisible()

    await activate(page, main.getByRole('button', { name: 'Disable user' }))
    const confirm = page.getByRole('alertdialog', { name: 'Disable user bob?' })
    await expect(confirm.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await activate(page, confirm.getByRole('button', { name: 'Disable user' }))
    await expect(page.getByText('User bob disabled').first()).toBeVisible()
    await expect(main.getByRole('button', { name: 'Enable user' })).toBeVisible()
  })

  test('explains a username that belongs to another source and leaves no password behind', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/users')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'Create local user' }))
    const dialog = page.getByRole('dialog', { name: 'Create local user' })
    await dialog.getByLabel(/^Username/).fill('alice')
    await dialog.getByLabel(/^Initial password/).fill('a long enough password')
    await dialog.getByLabel(/^Repeat the password/).fill('a long enough password')
    await dialog.getByLabel(/^Repeat the password/).press('Enter')
    await expect(
      dialog.getByText(/belongs to an account from another sign-in source/),
    ).toBeVisible()
    await expect(dialog.getByLabel(/^Initial password/)).toHaveValue('')
    await expect(dialog.getByLabel(/^Repeat the password/)).toHaveValue('')
    await expect(dialog.getByLabel(/^Username/)).toBeFocused()
  })

  test('creates a local user and lands on its page', async ({ page }) => {
    await signIn(page, 'admin', '/admin/users')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'Create local user' }))
    const dialog = page.getByRole('dialog', { name: 'Create local user' })
    await dialog.getByLabel(/^Username/).fill('zoe')
    await dialog.getByLabel(/^Initial password/).fill('a long enough password')
    await dialog.getByLabel(/^Repeat the password/).fill('a long enough password')
    await activate(page, dialog.getByRole('button', { name: 'Create user' }))
    await expect(page.getByRole('heading', { level: 1, name: 'zoe' })).toBeVisible()
    await expect(page).toHaveURL(/\/admin\/users\/[0-9a-f-]+$/)
  })

  test('shows an SSO account with the origin of every role and no password reset', async ({
    page,
  }) => {
    await signIn(page, 'admin', ALICE)
    const main = page.getByRole('main')
    await expect(main.getByText('OIDC group').first()).toBeVisible()
    await expect(main.getByText('Manual')).toBeVisible()
    await expect(main.getByText('Managed by the identity provider')).toBeVisible()
    await expect(main.getByRole('button', { name: 'Reset password' })).toHaveCount(0)
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/users')
      await expect(page.getByRole('main').getByRole('link', { name: 'alice' })).toBeVisible()
      await expectNoViolations(page, `users ${theme}`)
      await page.getByRole('main').getByRole('link', { name: 'alice' }).press('Enter')
      await expect(page.getByRole('heading', { level: 1, name: 'alice' })).toBeVisible()
      await expectNoViolations(page, `user detail ${theme}`)
    })
  }
})

screenshotSuite('users', [
  {
    name: 'list',
    path: '/admin/users',
    ready: async (page) => {
      await expect(page.getByRole('main').getByRole('link', { name: 'alice' })).toBeVisible()
    },
  },
  {
    name: 'detail',
    path: ALICE,
    ready: async (page) => {
      await expect(page.getByRole('main').getByText('OIDC group').first()).toBeVisible()
    },
  },
])
