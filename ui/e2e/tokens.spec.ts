import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  signIn,
  test,
} from './identity-support'

test.describe('tokens (keyboard only)', () => {
  test('creates a token, shows the secret once and forgets it, then revokes after a confirmation', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/me/tokens')
    const main = page.getByRole('main')
    await expect(main.getByText('ci-publisher').first()).toBeVisible()
    await activate(page, main.getByRole('button', { name: 'Create token' }))
    const form = page.getByRole('dialog', { name: 'Create token' })
    await form.getByLabel(/^Name/).fill('e2e-token')
    await form.getByLabel('Expires after').selectOption('30')
    await activate(page, form.getByRole('button', { name: 'Create token' }))

    const secretDialog = page.getByRole('dialog', { name: 'Copy your API token now' })
    await expect(secretDialog).toBeVisible()
    const secret = /ryx_[A-Za-z0-9]{43}/.exec((await secretDialog.textContent()) ?? '')?.[0]
    expect(secret).toBeTruthy()
    // It cannot be closed by accident before the person confirms that it was stored.
    await expect(secretDialog.getByRole('button', { name: 'Done' })).toBeDisabled()
    await page.keyboard.press('Escape')
    await expect(secretDialog).toBeVisible()
    await activate(
      page,
      secretDialog.getByRole('checkbox', { name: 'I stored it in a safe place' }),
      'Space',
    )
    await activate(page, secretDialog.getByRole('button', { name: 'Done' }))
    await expect(secretDialog).toHaveCount(0)
    // After that the secret is nowhere on the page, and the list shows only its prefix.
    await expect(page.getByText(secret!)).toHaveCount(0)
    expect(await page.evaluate(() => document.body.innerText)).not.toContain(secret)
    await expect(main.getByText('e2e-token')).toBeVisible()
    await expect(main.getByText(secret!.slice(0, 8)).first()).toBeVisible()

    await activate(page, main.getByRole('button', { name: 'Revoke token e2e-token' }))
    const confirm = page.getByRole('alertdialog', { name: 'Revoke token e2e-token?' })
    await expect(confirm.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await activate(page, confirm.getByRole('button', { name: 'Revoke token' }))
    await expect(page.getByText('Token e2e-token revoked').first()).toBeVisible()
  })

  test('keeps read and browse selected while write is, for custom scopes', async ({ page }) => {
    await signIn(page, 'admin', '/me/tokens')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'Create token' }))
    const form = page.getByRole('dialog', { name: 'Create token' })
    await form.getByLabel(/^Name/).fill('publisher')
    await activate(page, form.getByRole('radio', { name: 'Custom' }), 'Space')
    await form.getByLabel('Repository').selectOption('maven-releases')
    await activate(page, form.getByRole('checkbox', { name: 'Write' }), 'Space')
    await expect(form.getByRole('checkbox', { name: 'Read' })).toBeChecked()
    await expect(form.getByRole('checkbox', { name: 'Read' })).toBeDisabled()
  })

  test('lists every token for administrators and filters them from the address', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/tokens?state=revoked')
    const table = page.getByRole('main').getByRole('table')
    await expect(table.getByText('Revoked').first()).toBeVisible()
    await expect(table.getByText('Active')).toHaveCount(0)
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/me/tokens')
      await expect(page.getByRole('main').getByText('ci-publisher').first()).toBeVisible()
      await expectNoViolations(page, `my tokens ${theme}`)
      await page.goto('/admin/tokens')
      await expect(
        page.getByRole('main').getByRole('link', { name: 'alice' }).first(),
      ).toBeVisible()
      await expectNoViolations(page, `admin tokens ${theme}`)
    })
  }
})

screenshotSuite('tokens', [
  {
    name: 'mine',
    path: '/me/tokens',
    ready: async (page) => {
      await expect(page.getByRole('main').getByText('ci-publisher').first()).toBeVisible()
    },
  },
  {
    name: 'create',
    path: '/me/tokens?new=1',
    ready: async (page) => {
      await expect(page.getByRole('dialog', { name: 'Create token' })).toBeVisible()
    },
  },
  {
    name: 'admin',
    path: '/admin/tokens',
    ready: async (page) => {
      await expect(
        page.getByRole('main').getByRole('link', { name: 'alice' }).first(),
      ).toBeVisible()
    },
  },
])
