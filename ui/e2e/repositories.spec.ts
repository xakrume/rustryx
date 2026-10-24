import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations } from './system-helpers'

test.describe('repositories, browse, search and upload', () => {
  test('opens a repository and its content path', async ({ page }) => {
    await signIn(page, 'admin', '/repositories')
    await expect(page.getByRole('heading', { level: 1, name: 'Repositories' })).toBeVisible()
    await page.getByRole('link', { name: 'maven-releases', exact: true }).click()
    await expect(page.getByRole('heading', { level: 1, name: 'maven-releases' })).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Client setup' })).toBeVisible()
    await page.getByRole('link', { name: 'Browse', exact: true }).click()
    await expect(page.getByRole('tree', { name: 'Content of maven-releases' })).toBeVisible()
    await expect(page.getByRole('navigation', { name: 'Content path' })).toBeVisible()
  })

  test('names maven fields when that format is chosen', async ({ page }) => {
    await signIn(page, 'admin', '/search')
    await page.getByLabel('Format').selectOption('maven2')
    await expect(page.getByLabel('Group ID')).toBeVisible()
    await expect(page.getByLabel('Artifact ID')).toBeVisible()
  })

  test('explains that an image repository is published with its client', async ({ page }) => {
    await signIn(page, 'admin', '/upload?repository=docker-releases')
    await expect(page.getByText('Docker cannot be uploaded in the browser')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Client setup' })).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`content pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/repositories')
      await expect(page.getByRole('link', { name: 'maven-releases', exact: true })).toBeVisible()
      await expectNoViolations(page, `repositories ${theme}`)
      await page.goto('/repositories/maven-releases/browse')
      await expect(page.getByRole('tree', { name: 'Content of maven-releases' })).toBeVisible()
      await expectNoViolations(page, `browse ${theme}`)
      await page.goto('/search')
      await expect(page.getByRole('heading', { level: 1, name: 'Search' })).toBeVisible()
      await expectNoViolations(page, `search ${theme}`)
      await page.setViewportSize({ width: 375, height: 812 })
      expect(await hasHorizontalScroll(page)).toBe(false)
    })
  }
})
