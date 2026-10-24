import {
  activate,
  expect,
  expectNoViolations,
  preferTheme,
  screenshotSuite,
  test,
} from './identity-support'

test.describe('sign in page (keyboard only)', () => {
  test('signs in with the keyboard, keeps the username after a wrong password and focuses the password', async ({
    page,
  }) => {
    await page.goto('/login')
    await expect(page.getByLabel(/^Username/)).toBeFocused()
    await page.keyboard.type('admin')
    await page.keyboard.press('Tab')
    await expect(page.getByLabel(/^Password/)).toBeFocused()
    await page.keyboard.type('definitely-wrong')
    await page.keyboard.press('Enter')
    await expect(page.getByText('The username or password is not correct.')).toBeVisible()
    await expect(page.getByLabel(/^Username/)).toHaveValue('admin')
    await expect(page.getByLabel(/^Password/)).toBeFocused()
    await expect(page.getByLabel(/^Password/)).toHaveValue('')
    await page.keyboard.type('mock-password')
    await page.keyboard.press('Enter')
    await expect(page).toHaveURL('/')
    await expect(page.getByRole('heading', { level: 1, name: 'Home' })).toBeVisible()
  })

  test('shows the version, the About link and the theme choice on the page', async ({ page }) => {
    await page.goto('/login')
    await expect(page.locator('footer')).toContainText('Rustryx 0.1.0')
    await expect(page.getByRole('link', { name: 'About' })).toBeVisible()
    await activate(page, page.getByRole('button', { name: /^Theme:/ }))
    await page.getByRole('menuitemradio', { name: 'Dark' }).press('Enter')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  })

  test('counts a lock-out down visibly and blocks the button meanwhile', async ({ page }) => {
    await page.goto('/login')
    await page.getByLabel(/^Username/).fill('locked')
    await page.getByLabel(/^Password/).fill('x')
    await page.keyboard.press('Enter')
    await expect(page.getByText(/Try again in \d+ s\./)).toBeVisible()
    await expect(page.getByRole('button', { name: 'Sign in', exact: true })).toBeDisabled()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`has no accessibility violations, ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await page.goto('/login?error=oidc_callback_invalid&request_id=req-abc')
      await expect(page.getByText('req-abc')).toBeVisible()
      await expectNoViolations(page, `login ${theme}`)
    })
  }
})

screenshotSuite('login', [
  {
    name: 'form',
    path: '/login',
    ready: async (page) => {
      await expect(page.locator('footer')).toContainText('Rustryx 0.1.0')
    },
  },
  {
    name: 'callback-error',
    path: '/login?error=oidc_callback_invalid&request_id=0192f6c4-5a1e-7000-8000-00000000c0de',
    ready: async (page) => {
      await expect(page.getByText('0192f6c4-5a1e-7000-8000-00000000c0de')).toBeVisible()
    },
  },
])
