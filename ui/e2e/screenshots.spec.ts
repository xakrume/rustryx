import { preferTheme, signIn, test } from './fixtures'

/**
 * Writes the review screenshots to e2e/screenshots. Off by default so a normal run does not
 * rewrite committed files: `SCREENSHOTS=1 npm run test:e2e -- screenshots`.
 * Viewport size only (no full page), so the files stay small.
 */
test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')

const WIDTHS = [
  { name: '375', width: 375, height: 760 },
  { name: '768', width: 768, height: 800 },
  { name: '1440', width: 1440, height: 800 },
]

for (const theme of ['light', 'dark'] as const) {
  for (const size of WIDTHS) {
    test(`shell ${theme} ${size.name}`, async ({ page }) => {
      await preferTheme(page, theme)
      await page.setViewportSize({ width: size.width, height: size.height })
      await signIn(page, 'admin')
      await page.getByRole('heading', { name: 'Recent activity' }).waitFor()
      await page.screenshot({ path: `e2e/screenshots/shell-${theme}-${size.name}.png` })
    })
  }

  test(`login ${theme}`, async ({ page }) => {
    await preferTheme(page, theme)
    await page.setViewportSize({ width: 375, height: 760 })
    await page.goto('/login')
    await page.getByRole('heading', { name: 'Sign in to Rustryx' }).waitFor()
    await page.screenshot({ path: `e2e/screenshots/login-${theme}-375.png` })
  })

  test(`gallery ${theme}`, async ({ page }) => {
    await preferTheme(page, theme)
    await page.setViewportSize({ width: 1440, height: 800 })
    await signIn(page, 'admin', '/__components')
    await page.getByRole('cell', { name: 'maven-releases', exact: true }).waitFor()
    await page.screenshot({ path: `e2e/screenshots/gallery-${theme}-1440.png` })
  })
}
