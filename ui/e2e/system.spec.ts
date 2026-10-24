import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations, setStatusScenario, tabTo, WIDTHS } from './system-helpers'

test.describe('system status', () => {
  test('must_keep_readiness_ready_while_dependencies_are_down', async ({ page }) => {
    await signIn(page, 'admin', '/admin/system/status')
    await setStatusScenario(page, 'dependencies-down')
    await page.reload()
    const readiness = page
      .getByRole('heading', { name: 'Readiness of this node' })
      .locator('xpath=ancestor::div[contains(@class,"rounded-lg")][1]')
    await expect(readiness.getByText('Ready', { exact: true })).toBeVisible()
    await expect(page.getByText('Not ready')).toHaveCount(0)
    const deps = page.getByRole('list', { name: 'Dependency health' })
    await expect(deps.getByText('Unreachable').first()).toBeVisible()
    await expect(page.getByText(/never changes the readiness of this node/)).toBeVisible()
  })

  test('says not ready only when the readiness probe says so', async ({ page }) => {
    await signIn(page, 'admin', '/admin/system/status')
    await setStatusScenario(page, 'not-ready')
    await page.reload()
    await expect(page.getByText('Not ready', { exact: true })).toBeVisible()
    await expect(page.getByText('PostgreSQL answers')).toBeVisible()
  })

  test('shows what needs attention, the nodes and refreshes by keyboard', async ({ page }) => {
    await signIn(page, 'admin', '/admin/system/status')
    await setStatusScenario(page, 'mixed')
    await page.reload()
    await expect(page.getByText('Some checks need attention')).toBeVisible()
    await expect(page.getByText(/How to fix: Raise the quota/)).toBeVisible()
    const nodes = page.getByRole('region', { name: 'Nodes' })
    await expect(nodes.getByText('rustryx-1')).toBeVisible()
    await expect(nodes.getByText('This node')).toBeVisible()
    await expect(page.getByText(/Database schema version 42/)).toBeVisible()
    await tabTo(page, 'Refresh')
    await page.keyboard.press('Enter')
    await expect(page.getByText('Some checks need attention')).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/system/status')
      await setStatusScenario(page, 'mixed')
      await page.reload()
      await expect(page.getByText('Some checks need attention')).toBeVisible()
      await expect(page.getByText('Server errors (5xx)')).toBeVisible()
      await expectNoViolations(page, `status mixed ${theme}`)
      await setStatusScenario(page, 'healthy')
      await page.reload()
      await expect(page.getByText('All checks passed')).toBeVisible()
      await expectNoViolations(page, `status healthy ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/system/status')
    await setStatusScenario(page, 'mixed')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      await page.goto('/admin/system/status')
      await expect(page.getByText('Server errors (5xx)')).toBeVisible()
      expect(await hasHorizontalScroll(page), `status at ${size.name}`).toBe(false)
    }
  })
})

test.describe('about page', () => {
  test('is public and shows version, build and the signature hint', async ({ page }) => {
    await page.goto('/about')
    await expect(page.getByRole('heading', { level: 1, name: 'About Rustryx' })).toBeVisible()
    await expect(page.getByText('0.1.0').first()).toBeVisible()
    await expect(page.getByText('cosign verify ghcr.io/example/rustryx:0.1.0')).toBeVisible()
    await expect(page.getByText('This server does not publish an SBOM.')).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`passes axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await page.goto('/about')
      await expect(page.getByRole('heading', { level: 1, name: 'About Rustryx' })).toBeVisible()
      await expectNoViolations(page, `about ${theme}`)
    })
  }
})

test.describe('about page', () => {
  for (const theme of ['light', 'dark'] as const) {
    test(`is readable without signing in and passes axe in the ${theme} theme`, async ({
      page,
    }) => {
      await preferTheme(page, theme)
      await page.goto('/about')
      await expect(page.getByRole('heading', { level: 1, name: 'About Rustryx' })).toBeVisible()
      await expect(page.getByText('cosign verify')).toBeVisible()
      await expectNoViolations(page, `about ${theme}`)
    })
  }
})

test.describe('system review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`status ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/system/status')
        await setStatusScenario(page, 'mixed')
        await page.reload()
        await expect(page.getByText('Server errors (5xx)')).toBeVisible()
        await page.screenshot({
          path: `e2e/screenshots/system/status-top-${theme}-${size.name}.png`,
        })
        await page.getByRole('region', { name: 'Nodes' }).scrollIntoViewIfNeeded()
        await page.screenshot({
          path: `e2e/screenshots/system/status-nodes-${theme}-${size.name}.png`,
        })
        await page.goto('/about')
        await expect(page.getByText('cosign verify')).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/system/about-${theme}-${size.name}.png` })
        await page.goto('/admin/system/status')
        await setStatusScenario(page, 'dependencies-down')
        await page.reload()
        await expect(page.getByText('Some checks failed')).toBeVisible()
        await page.screenshot({
          path: `e2e/screenshots/system/status-deps-down-${theme}-${size.name}.png`,
        })
      })
    }
  }
})
