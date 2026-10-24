import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations, tabTo, WIDTHS } from './system-helpers'

const MAVEN_POLICY = '0192f6c4-5a1e-7000-8002-000000000001'

test.describe('cleanup policies', () => {
  test('creates a policy by keyboard, previews it and exports the plan', async ({ page }) => {
    await signIn(page, 'admin', '/admin/cleanup-policies')
    await expect(page.getByRole('link', { name: 'maven-old-snapshots', exact: true })).toBeVisible()

    await tabTo(page, 'Create policy')
    await page.keyboard.press('Enter')
    await expect(
      page.getByRole('heading', { level: 1, name: 'Create a cleanup policy' }),
    ).toBeVisible()

    await page.getByLabel(/^Name/).focus()
    await page.keyboard.type('e2e-old-snapshots')
    await page.getByLabel(/^Older than/).focus()
    await page.keyboard.type('45')
    await page.keyboard.press('Enter')

    await expect(page.getByText('Policy e2e-old-snapshots created', { exact: true })).toBeVisible()
    const preview = page.getByRole('button', { name: 'Preview what would be removed' })
    await expect(preview).toBeEnabled()
    await tabTo(page, 'Preview what would be removed')
    await page.keyboard.press('Enter')

    // The run is queued, planning, then done: the report refreshes on its own.
    await expect(page.getByText('Planning what would be removed')).toBeVisible()
    await expect(page.getByText('Items to remove')).toBeVisible({ timeout: 20_000 })
    await expect(page.getByText('Reclaimable size')).toBeVisible()
    const table = page.getByRole('region', { name: 'Items of the plan' })
    await expect(table.getByText(/com\.example\.payments:ledger/).first()).toBeVisible()

    // The CSV is a download: the strict policy must not block it.
    const [download] = await Promise.all([
      page.waitForEvent('download'),
      page.getByRole('button', { name: 'Export CSV' }).click(),
    ])
    expect(download.suggestedFilename()).toMatch(/^cleanup-plan-.*\.csv$/)
  })

  test('disables a criterion the chosen format cannot use and says why', async ({ page }) => {
    await signIn(page, 'admin', '/admin/cleanup-policies?edit=new')
    await expect(page.getByRole('option', { name: 'Raw' })).toBeAttached()
    await page.getByLabel(/^Format/).selectOption('raw')
    await expect(page.getByLabel(/^Not downloaded for/)).toBeDisabled()
    await expect(page.getByText('Not supported for Raw repositories.').first()).toBeVisible()
  })

  test('must_not_offer_the_preview_while_the_form_has_unsaved_changes', async ({ page }) => {
    await signIn(page, 'admin', `/admin/cleanup-policies?edit=${MAVEN_POLICY}`)
    const preview = page.getByRole('button', { name: 'Preview what would be removed' })
    await expect(preview).toBeEnabled()
    await page.getByLabel(/^Older than/).fill('91')
    await expect(preview).toBeDisabled()
    await expect(page.getByText(/Save your changes first/)).toBeVisible()
  })

  test('names the repositories a deleted policy is detached from', async ({ page }) => {
    await signIn(page, 'admin', '/admin/cleanup-policies')
    await page.getByRole('button', { name: 'Delete npm-stale-packages' }).click()
    const dialog = page.getByRole('alertdialog')
    await expect(dialog.getByText(/2 repositories use it/)).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Detach and delete' })).toBeDisabled()
    await dialog.getByLabel('Confirmation text').fill('npm-stale-packages')
    await dialog.getByRole('button', { name: 'Detach and delete' }).click()
    await expect(page.getByText('Policy npm-stale-packages deleted', { exact: true })).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/cleanup-policies')
      await expect(
        page.getByRole('link', { name: 'maven-old-snapshots', exact: true }),
      ).toBeVisible()
      await expectNoViolations(page, `cleanup list ${theme}`)
      await page.goto(`/admin/cleanup-policies?edit=${MAVEN_POLICY}`)
      await page.getByRole('button', { name: 'Preview what would be removed' }).click()
      await expect(page.getByText('Items to remove')).toBeVisible({ timeout: 20_000 })
      await expect(page.getByRole('region', { name: 'Items of the plan' })).toBeVisible()
      await expectNoViolations(page, `cleanup editor with report ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/cleanup-policies')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      await page.goto('/admin/cleanup-policies')
      await expect(
        page.getByRole('link', { name: 'maven-old-snapshots', exact: true }),
      ).toBeVisible()
      expect(await hasHorizontalScroll(page), `list at ${size.name}`).toBe(false)
      await page.goto(`/admin/cleanup-policies?edit=${MAVEN_POLICY}`)
      await expect(page.getByLabel(/^Older than/)).toBeVisible()
      expect(await hasHorizontalScroll(page), `editor at ${size.name}`).toBe(false)
    }
  })
})

test.describe('cleanup review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`cleanup ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/cleanup-policies')
        await expect(
          page.getByRole('link', { name: 'maven-old-snapshots', exact: true }),
        ).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/cleanup/list-${theme}-${size.name}.png` })
        await page.goto(`/admin/cleanup-policies?edit=${MAVEN_POLICY}`)
        await expect(page.getByLabel(/^Older than/)).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/cleanup/editor-${theme}-${size.name}.png` })
        await page.getByRole('button', { name: 'Preview what would be removed' }).click()
        await expect(page.getByText('Items to remove')).toBeVisible({ timeout: 20_000 })
        await page.getByText('Items to remove').scrollIntoViewIfNeeded()
        await page.screenshot({ path: `e2e/screenshots/cleanup/preview-${theme}-${size.name}.png` })
      })
    }
  }
})
