import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  signIn,
  test,
} from './identity-support'

test.describe('own profile (keyboard only)', () => {
  test('opens from the account menu and shows roles with their origin and the permission summary', async ({
    page,
  }) => {
    await signIn(page, 'admin')
    await activate(page, page.getByRole('button', { name: /Account menu/ }))
    await page.getByRole('menuitem', { name: 'Profile' }).press('Enter')
    await expect(page).toHaveURL('/me')
    await expect(page.getByRole('heading', { level: 1, name: 'Profile' })).toBeFocused()
    const main = page.getByRole('main')
    await expect(main.getByText('nx-admin').first()).toBeVisible()
    await expect(main.getByText('Manual')).toBeVisible()
    await expect(main.getByText(/You hold nx-admin/)).toBeVisible()
    await expect(main.getByText('Audit log', { exact: true })).toBeVisible()
  })

  test('must_not_keep_a_password_on_the_page_after_saving, and a wrong current password stays at its field', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/me')
    const main = page.getByRole('main')
    await main.getByLabel(/^Current password/).fill('not the password')
    await main.getByLabel(/^New password/).fill('a brand new password 1')
    await main.getByLabel(/^Repeat the new password/).fill('a brand new password 1')
    await activate(page, main.getByRole('button', { name: 'Change password' }))
    await expect(main.getByText('The current password is not correct.')).toBeVisible()
    await expect(main.getByLabel(/^Current password/)).toBeFocused()
    await expect(main.getByLabel(/^New password/)).toHaveValue('')
    await main.getByLabel(/^Current password/).fill('mock-password')
    await main.getByLabel(/^New password/).fill('a brand new password 1')
    await main.getByLabel(/^Repeat the new password/).fill('a brand new password 1')
    await activate(page, main.getByRole('button', { name: 'Change password' }))
    await expect(
      page.getByText('Password changed. Your other sessions ended.').first(),
    ).toBeVisible()
    for (const label of [/^Current password/, /^New password/, /^Repeat the new password/]) {
      await expect(main.getByLabel(label)).toHaveValue('')
    }
    await expect(page.getByText('a brand new password 1')).toHaveCount(0)
  })

  test('shows the password for an SSO account as managed elsewhere', async ({ page }) => {
    await page.goto('/login')
    await page.evaluate(() => {
      window.sessionStorage.setItem(
        'ryx-mock-session',
        JSON.stringify({
          username: 'alice',
          csrf: 'c',
          issuedAt: Date.now(),
          touchedAt: Date.now(),
        }),
      )
    })
    await page.goto('/me')
    await expect(page.getByText('Your password is managed elsewhere')).toBeVisible()
    await expect(page.getByText('OIDC group').first()).toBeVisible()
  })

  test('signs out everywhere after a confirmation and lands on the sign in page', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/me')
    await activate(page, page.getByRole('button', { name: 'Sign out everywhere' }))
    const dialog = page.getByRole('alertdialog', { name: 'Sign out everywhere?' })
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await activate(page, dialog.getByRole('button', { name: 'Sign out everywhere' }))
    await expect(page).toHaveURL(/\/login/)
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/me')
      await expect(page.getByText(/You hold nx-admin/)).toBeVisible()
      await expectNoViolations(page, `profile ${theme}`)
    })
  }
})

screenshotSuite('profile', [
  {
    name: 'profile',
    path: '/me',
    ready: async (page) => {
      await expect(page.getByText(/You hold nx-admin/)).toBeVisible()
    },
  },
])
