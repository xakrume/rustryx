import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations, tabTo, WIDTHS } from './system-helpers'

const BLOB_GC = '0192f6c4-5a1e-7000-8007-000000000001'
const BLOB_GC_RUN = '0192f6c4-5a1e-7000-8008-000000000064'
const FLUSH_RUN = '0192f6c4-5a1e-7000-8008-0000000001f4'
const MAVEN_DRY_RUN = '0192f6c4-5a1e-7000-8003-000000000001'

test.describe('tasks and lifecycle runs', () => {
  test('opens a task, its run history and a run log by keyboard', async ({ page }) => {
    await signIn(page, 'admin', '/admin/tasks')
    await expect(page.getByRole('heading', { level: 1, name: 'Tasks' })).toBeVisible()
    await expect(page.getByText('Every day').first()).toBeVisible()

    await tabTo(page, 'Open task Blob garbage collection', 'link')
    await page.keyboard.press('Enter')
    await expect(
      page.getByRole('heading', { level: 1, name: 'Blob garbage collection' }),
    ).toBeVisible()
    await expect(page.getByText('Next runs')).toBeVisible()

    const history = page.getByRole('region', { name: 'Runs of Blob garbage collection' })
    const firstRun = history.getByRole('link', { name: /^Open run from/ }).first()
    await expect(firstRun).toBeVisible()
    await firstRun.focus()
    await page.keyboard.press('Enter')

    await expect(
      page.getByRole('heading', { level: 1, name: 'Run of Blob garbage collection' }),
    ).toBeVisible()
    const log = page.getByRole('log', { name: 'Log of the run' })
    await expect(log.getByText('Starting blob-gc')).toBeVisible()
    await expect(page.getByText(/End of the log/)).toBeVisible()
  })

  test('follows a running run until it ends', async ({ page }) => {
    await signIn(
      page,
      'admin',
      `/admin/tasks/0192f6c4-5a1e-7000-8007-000000000005/runs/${FLUSH_RUN}`,
    )
    await expect(page.getByText(/The run is in progress/)).toBeVisible()
    await expect(page.getByText(/End of the log/)).toBeVisible({ timeout: 30_000 })
  })

  test('shows lifecycle runs and the dry run report of one', async ({ page }) => {
    await signIn(page, 'admin', '/admin/tasks')
    await page.getByRole('tab', { name: 'Lifecycle runs' }).click()
    const table = page.getByRole('region', { name: 'Lifecycle runs' })
    await expect(table.getByText('Dry run').first()).toBeVisible()
    await table.getByRole('button', { name: 'Open report of maven-old-snapshots' }).first().click()
    await expect(page.getByText('Items to remove')).toBeVisible()
    await expect(page.getByText('412', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Back to runs' }).click()
    await expect(page.getByRole('region', { name: 'Lifecycle runs' })).toBeVisible()
  })

  test('must_say_that_an_expired_report_is_gone_instead_of_showing_old_numbers', async ({
    page,
  }) => {
    await signIn(
      page,
      'admin',
      '/admin/tasks?view=lifecycle&run=0192f6c4-5a1e-7000-8003-000000000005',
    )
    await expect(page.getByText('This report is no longer available')).toBeVisible()
    await expect(page.getByRole('region', { name: 'Items of the plan' })).toHaveCount(0)
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/tasks')
      await expect(page.getByText('Every day').first()).toBeVisible()
      await expectNoViolations(page, `tasks list ${theme}`)
      await page.goto(`/admin/tasks?task=${BLOB_GC}`)
      await expect(page.getByText('Next runs')).toBeVisible()
      await expect(
        page
          .getByRole('region', { name: 'Runs of Blob garbage collection' })
          .getByRole('link')
          .first(),
      ).toBeVisible()
      await expectNoViolations(page, `task detail ${theme}`)
      await page.goto(`/admin/tasks/${BLOB_GC}/runs/${BLOB_GC_RUN}`)
      await expect(page.getByText(/End of the log/)).toBeVisible()
      await expectNoViolations(page, `task run ${theme}`)
      await page.goto(`/admin/tasks?view=lifecycle&run=${MAVEN_DRY_RUN}`)
      await expect(page.getByRole('region', { name: 'Items of the plan' })).toBeVisible()
      await expectNoViolations(page, `lifecycle report ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/tasks')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      for (const path of [
        '/admin/tasks',
        `/admin/tasks?task=${BLOB_GC}`,
        `/admin/tasks/${BLOB_GC}/runs/${BLOB_GC_RUN}`,
        `/admin/tasks?view=lifecycle&run=${MAVEN_DRY_RUN}`,
      ]) {
        await page.goto(path)
        await expect(page.getByRole('main')).toBeVisible()
        await page.waitForTimeout(400)
        expect(await hasHorizontalScroll(page), `${path} at ${size.name}`).toBe(false)
      }
    }
  })
})

test.describe('tasks review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`tasks ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/tasks')
        await expect(page.getByText('Every day').first()).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/tasks/list-${theme}-${size.name}.png` })
        await page.goto(`/admin/tasks/${BLOB_GC}/runs/${BLOB_GC_RUN}`)
        await expect(page.getByText(/End of the log/)).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/tasks/run-${theme}-${size.name}.png` })
        await page.goto(`/admin/tasks?view=lifecycle&run=${MAVEN_DRY_RUN}`)
        await expect(page.getByRole('region', { name: 'Items of the plan' })).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/tasks/report-${theme}-${size.name}.png` })
      })
    }
  }
})
