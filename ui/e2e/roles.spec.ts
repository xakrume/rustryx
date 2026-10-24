import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  signIn,
  test,
} from './identity-support'

const RELEASE_MANAGERS = '/admin/roles?edit=0192f6c4-5a1e-7000-8000-000000000012'

test.describe('roles (keyboard only)', () => {
  test('creates a role with a privilege and finds it in the list', async ({ page }) => {
    await signIn(page, 'admin', '/admin/roles')
    const main = page.getByRole('main')
    await activate(page, main.getByRole('button', { name: 'Create role' }))
    const dialog = page.getByRole('dialog', { name: 'Create role' })
    await dialog.getByLabel(/^Name/).fill('qa-team')
    await activate(page, dialog.getByRole('checkbox', { name: 'npm-read' }), 'Space')
    await activate(page, dialog.getByRole('button', { name: 'Create role' }))
    await expect(page.getByText('Role qa-team created').first()).toBeVisible()
    await expect(main.getByRole('button', { name: 'qa-team' })).toBeVisible()
    // The save leaves a clean form: closing must not ask to discard anything.
    await expect(page.getByRole('alertdialog')).toHaveCount(0)
  })

  test('must_ask_before_deleting_a_role_in_use_and_name_it, then deletes it after the name was typed', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/roles')
    const main = page.getByRole('main')
    await activate(page, main.getByRole('button', { name: 'Delete role maven-publishers' }))
    const dialog = page.getByRole('alertdialog', { name: /Delete role\s+maven-publishers\s*\?/ })
    await expect(dialog).toContainText('in use')
    const confirm = dialog.getByRole('button', { name: 'Delete role' })
    await expect(confirm).toBeDisabled()
    await dialog.getByRole('textbox', { name: 'Confirmation text' }).fill('maven-publishers')
    await activate(page, confirm)
    await expect(page.getByText('Role maven-publishers deleted').first()).toBeVisible()
    await expect(main.getByRole('button', { name: 'maven-publishers' })).toHaveCount(0)
  })

  test('does not offer a nested role that would form a cycle, and says why', async ({ page }) => {
    await signIn(page, 'admin', '/admin/roles')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'maven-publishers' }))
    const dialog = page.getByRole('dialog', { name: /Edit role maven-publishers/ })
    await expect(dialog.getByRole('checkbox', { name: 'release-managers' })).toBeDisabled()
    await expect(
      dialog.getByText('Would create a cycle: release-managers includes maven-publishers'),
    ).toBeVisible()
  })

  test('opens a built-in role read only', async ({ page }) => {
    await signIn(page, 'admin', '/admin/roles')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'View role nx-admin' }))
    const dialog = page.getByRole('dialog', { name: /Role nx-admin/ })
    await expect(dialog.getByText(/built-in role/)).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Save role' })).toHaveCount(0)
  })

  test('asks before discarding edits and keeps them when told to stay', async ({ page }) => {
    await signIn(page, 'admin', '/admin/roles')
    await activate(page, page.getByRole('main').getByRole('button', { name: 'npm-readers' }))
    const dialog = page.getByRole('dialog', { name: /Edit role npm-readers/ })
    await dialog.getByLabel('Description').fill('changed')
    await activate(page, dialog.getByRole('button', { name: 'Cancel' }))
    const guard = page.getByRole('alertdialog', { name: 'Discard unsaved changes?' })
    await expect(guard.getByRole('button', { name: 'Keep editing' })).toBeFocused()
    await page.keyboard.press('Enter')
    await expect(dialog.getByLabel('Description')).toHaveValue('changed')
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/roles')
      await expect(
        page.getByRole('main').getByRole('button', { name: 'maven-publishers' }),
      ).toBeVisible()
      await expectNoViolations(page, `roles ${theme}`)
      await activate(page, page.getByRole('main').getByRole('button', { name: 'release-managers' }))
      await expect(page.getByRole('dialog', { name: /Edit role release-managers/ })).toBeVisible()
      await expectNoViolations(page, `role editor ${theme}`)
    })
  }
})

screenshotSuite('roles', [
  {
    name: 'list',
    path: '/admin/roles',
    ready: async (page) => {
      await expect(
        page.getByRole('main').getByRole('button', { name: 'maven-publishers' }),
      ).toBeVisible()
    },
  },
  {
    name: 'editor',
    path: RELEASE_MANAGERS,
    ready: async (page) => {
      await page.getByRole('dialog').getByRole('checkbox', { name: 'maven-publishers' }).waitFor()
    },
  },
])
