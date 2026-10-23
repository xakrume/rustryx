import AxeBuilder from '@axe-core/playwright'
import type { Page } from '@playwright/test'
import { expect, preferTheme, signIn, test } from './fixtures'

/** axe in a real browser, so color contrast is measured on the real rendering of both themes. */
async function expectNoViolations(page: Page, label: string): Promise<void> {
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'])
    .analyze()
  const summary = results.violations.map(
    (v) =>
      `${v.id} (${v.impact}): ${v.nodes
        .map((n) => n.target.join(' '))
        .slice(0, 3)
        .join(' | ')}`,
  )
  expect(summary, label).toEqual([])
}

for (const theme of ['light', 'dark'] as const) {
  test.describe(`accessibility, ${theme} theme`, () => {
    test.beforeEach(async ({ page }) => {
      await preferTheme(page, theme)
    })

    test('login page', async ({ page }) => {
      await page.goto('/login')
      await expect(page.getByRole('heading', { name: 'Sign in to Rustryx' })).toBeVisible()
      await expectNoViolations(page, `login ${theme}`)
    })

    test('login page with errors', async ({ page }) => {
      await page.goto('/login?error=oidc_callback_invalid&request_id=req-abc&reason=expired')
      await page.getByLabel(/^Username/).fill('admin')
      await page.getByLabel(/^Password/).fill('wrong')
      await page.getByRole('button', { name: 'Sign in', exact: true }).click()
      await expect(page.getByText('The username or password is not correct.')).toBeVisible()
      await expectNoViolations(page, `login errors ${theme}`)
    })

    test('home as administrator, desktop', async ({ page }) => {
      await signIn(page)
      await expect(page.getByRole('heading', { name: 'Recent activity' })).toBeVisible()
      await expectNoViolations(page, `home ${theme}`)
    })

    test('home as administrator, phone with the drawer open', async ({ page }) => {
      await page.setViewportSize({ width: 375, height: 812 })
      await signIn(page)
      await expect(page.getByRole('heading', { name: 'Recent activity' })).toBeVisible()
      await expectNoViolations(page, `home phone ${theme}`)
      await page.getByRole('button', { name: 'Open navigation menu' }).click()
      await expect(page.getByRole('dialog')).toBeVisible()
      await expectNoViolations(page, `drawer ${theme}`)
    })

    test('command palette', async ({ page }) => {
      await signIn(page)
      await page.keyboard.press('Control+K')
      await expect(page.getByRole('dialog', { name: 'Command palette' })).toBeVisible()
      await expectNoViolations(page, `palette ${theme}`)
    })

    test('forbidden page and 404', async ({ page }) => {
      await page.goto('/definitely/not/here')
      await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible()
      await expectNoViolations(page, `404 ${theme}`)
    })

    test('component gallery', async ({ page }) => {
      await signIn(page, 'admin', '/__components')
      await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toBeVisible()
      await expectNoViolations(page, `gallery ${theme}`)
    })

    test('component gallery with dialogs and toasts open', async ({ page }) => {
      await signIn(page, 'admin', '/__components')
      await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toBeVisible()
      await page.getByRole('button', { name: 'API error toast' }).click()
      await expect(page.getByRole('region', { name: 'Notifications' })).toContainText(
        'internal_error',
      )
      await expectNoViolations(page, `toast ${theme}`)
      await page.getByRole('button', { name: 'Typed confirmation' }).click()
      await expect(page.getByRole('alertdialog')).toBeVisible()
      await expectNoViolations(page, `confirm ${theme}`)
    })
  })
}
