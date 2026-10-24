import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations, tabTo, WIDTHS } from './system-helpers'

test.describe('system settings', () => {
  test('changes a token lifetime by keyboard and keeps configuration values read only', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/settings?section=security')
    await expect(page.getByRole('heading', { level: 1, name: 'Settings' })).toBeVisible()
    const readOnly = page.getByRole('region', { name: 'Read only' })
    await expect(readOnly.getByText('1800')).toBeVisible()
    await expect(page.getByLabel(/Session idle timeout/)).toHaveCount(0)

    const field = page.getByLabel(/Default token lifetime/)
    await field.focus()
    await page.keyboard.press('ControlOrMeta+A')
    await page.keyboard.type('60')
    await page.keyboard.press('Enter')
    await expect(page.getByText('Settings saved', { exact: true })).toBeVisible()
    await expect(field).toHaveValue('60')
  })

  test('must_ask_before_turning_on_read_only_mode', async ({ page }) => {
    await signIn(page, 'admin', '/admin/settings')
    const toggle = page.getByRole('switch', { name: /Read only mode/ })
    await toggle.focus()
    await page.keyboard.press('Space')
    await expect(toggle).toBeChecked()
    await tabTo(page, 'Save changes')
    await page.keyboard.press('Enter')
    const dialog = page.getByRole('alertdialog')
    await expect(dialog.getByText('Turn on read only mode?')).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Keep it off' })).toBeFocused()
    await page.keyboard.press('Enter')
    await expect(dialog).toHaveCount(0)
    await expect(page.getByText('Settings saved')).toHaveCount(0)
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/settings')
      await expect(page.getByRole('switch', { name: /Read only mode/ })).toBeVisible()
      await expectNoViolations(page, `settings general ${theme}`)
      await page.goto('/admin/settings?section=security')
      await expect(page.getByLabel(/Default token lifetime/)).toBeVisible()
      await expectNoViolations(page, `settings security ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/settings?section=security')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      await page.goto('/admin/settings?section=security')
      await expect(page.getByLabel(/Default token lifetime/)).toBeVisible()
      expect(await hasHorizontalScroll(page), `settings at ${size.name}`).toBe(false)
    }
  })
})

test.describe('settings review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`settings ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/settings?section=security')
        await expect(page.getByLabel(/Default token lifetime/)).toBeVisible()
        await page.screenshot({
          path: `e2e/screenshots/settings/security-${theme}-${size.name}.png`,
        })
      })
    }
  }
})
