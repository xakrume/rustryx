import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  signIn,
  test,
} from './identity-support'

test.describe('privileges (keyboard only)', () => {
  test('creates a repository privilege, taking the format from the repository, and warns about a wildcard', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/privileges')
    const main = page.getByRole('main')
    await activate(page, main.getByRole('button', { name: 'Create privilege' }))
    const dialog = page.getByRole('dialog', { name: 'Create privilege' })
    await dialog.getByLabel(/^Name/).fill('maven-snapshots-read')
    await dialog.getByLabel(/^Repository/).selectOption('maven-snapshots')
    await expect(dialog.getByLabel('Format')).toHaveValue('maven2')
    await expect(dialog.getByLabel('Format')).toBeDisabled()
    await dialog.getByLabel(/^Repository/).selectOption('*')
    await expect(dialog.getByText(/applies to every repository/)).toBeVisible()
    await activate(page, dialog.getByRole('checkbox', { name: 'Write' }), 'Space')
    await expect(dialog.getByText(/Write or delete on every repository/)).toBeVisible()
    await dialog.getByLabel(/^Repository/).selectOption('maven-snapshots')
    await activate(page, dialog.getByRole('button', { name: 'Create privilege' }))
    await expect(page.getByText('Privilege maven-snapshots-read created').first()).toBeVisible()
    await expect(main.getByRole('button', { name: 'maven-snapshots-read' })).toBeVisible()
  })

  test('must_not_save_a_privilege_without_an_action', async ({ page }) => {
    await signIn(page, 'admin', '/admin/privileges')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'Create privilege' }))
    const dialog = page.getByRole('dialog', { name: 'Create privilege' })
    await dialog.getByLabel(/^Name/).fill('nothing')
    await dialog.getByLabel(/^Repository/).selectOption('*')
    await activate(page, dialog.getByRole('checkbox', { name: 'Browse' }), 'Space')
    await activate(page, dialog.getByRole('checkbox', { name: 'Read' }), 'Space')
    await activate(page, dialog.getByRole('button', { name: 'Create privilege' }))
    await expect(
      dialog.getByText('Select at least one action: a privilege without actions is refused.'),
    ).toBeVisible()
  })

  test('says why the admin grant cannot be created and lists the roles before a delete', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/privileges')
    const main = page.getByRole('main')
    await activate(page, main.getByRole('button', { name: 'Create privilege' }))
    const dialog = page.getByRole('dialog', { name: 'Create privilege' })
    await expect(
      dialog.getByRole('radio', { name: 'Administration (built-in only)' }),
    ).toBeDisabled()
    await page.keyboard.press('Escape')
    await activate(page, main.getByRole('button', { name: 'Delete privilege npm-read' }))
    const confirm = page.getByRole('alertdialog', { name: /Delete privilege\s+npm-read\s*\?/ })
    await expect(confirm).toContainText('npm-readers')
    await expect(confirm.getByRole('button', { name: 'Delete privilege' })).toBeDisabled()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/privileges')
      await expect(page.getByRole('main').getByRole('button', { name: 'npm-read' })).toBeVisible()
      await expectNoViolations(page, `privileges ${theme}`)
      await activate(page, page.getByRole('main').getByRole('button', { name: 'Create privilege' }))
      await expect(page.getByRole('dialog', { name: 'Create privilege' })).toBeVisible()
      await expectNoViolations(page, `privilege editor ${theme}`)
    })
  }
})

screenshotSuite('privileges', [
  {
    name: 'list',
    path: '/admin/privileges',
    ready: async (page) => {
      await expect(page.getByRole('main').getByRole('button', { name: 'npm-read' })).toBeVisible()
    },
  },
  {
    name: 'editor',
    path: '/admin/privileges?edit=0192f6c4-5a1e-7000-8000-000000000204',
    ready: async (page) => {
      await expect(page.getByRole('dialog').getByLabel(/^Name/)).toHaveValue('maven-releases-write')
    },
  },
])
