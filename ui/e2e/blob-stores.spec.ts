import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { clearFaults, expectNoViolations, setFaults, tabTo, WIDTHS } from './system-helpers'

test.describe('blob stores', () => {
  test('creates a local blob store by keyboard and lands on its page', async ({ page }) => {
    await signIn(page, 'admin', '/admin/blob-stores')
    await expect(page.getByRole('heading', { level: 1, name: 'Blob stores' })).toBeVisible()
    await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()

    await tabTo(page, 'Create blob store')
    await page.keyboard.press('Enter')
    await expect(page.getByRole('heading', { level: 1, name: 'Create a blob store' })).toBeVisible()

    await page.getByLabel(/^Name/).focus()
    await page.keyboard.type('e2e-store')
    await page.keyboard.press('Tab')
    // Kind (a native select) keeps "Local directory"; the next field is the directory.
    await page.keyboard.press('Tab')
    await page.keyboard.type('/mnt/e2e/blobs')
    await page.keyboard.press('Enter')

    await expect(page.getByRole('heading', { level: 1, name: 'e2e-store' })).toBeVisible()
    await expect(page.getByText('Blob store e2e-store created', { exact: true })).toBeVisible()
    await expect(page.getByLabel(/^Directory/)).toHaveValue('/mnt/e2e/blobs')
  })

  test('must_not_put_the_s3_secret_key_in_the_page_after_saving', async ({ page }) => {
    const secret = 'e2e-secret-key-7d41'
    await signIn(page, 'admin', '/admin/blob-stores?view=create')
    await page.getByLabel(/^Name/).fill('secure-e2e')
    await page.getByLabel(/^Kind/).selectOption('s3')
    await page.getByLabel(/^Endpoint URL/).fill('https://seaweedfs.example.org')
    await page.getByLabel(/^Bucket/).fill('secure')
    await page.getByLabel(/^Region/).fill('us-east-1')
    await page.getByLabel(/^Credentials/).selectOption('inline')
    await page.getByLabel(/^Access key(?! reference)/).fill('AKIAE2E')
    await page.getByLabel(/^Secret key(?! reference)/).fill(secret)
    await expect(page.getByLabel(/^Secret key(?! reference)/)).toHaveAttribute('type', 'password')
    await page.getByRole('button', { name: 'Create blob store' }).click()
    await expect(page.getByRole('heading', { level: 1, name: 'secure-e2e' })).toBeVisible()
    await expect(page.getByText(/Credentials: configured/)).toBeVisible()
    const html = await page.content()
    expect(html).not.toContain(secret)
    const values = await page
      .locator('input')
      .evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value))
    expect(values.join('|')).not.toContain(secret)
  })

  test('must_block_deleting_a_store_that_repositories_use_and_say_why', async ({ page }) => {
    await signIn(page, 'admin', '/admin/blob-stores/fast-ssd')
    await expect(page.getByRole('heading', { level: 1, name: 'fast-ssd' })).toBeVisible()
    const button = page.getByRole('button', { name: 'Delete blob store' })
    await expect(button).toBeDisabled()
    await expect(page.getByText('This blob store is in use')).toBeVisible()
    await expect(page.getByText(/maven-releases, docker-releases/).first()).toBeVisible()
  })

  test('shows the error code and request id and recovers on retry', async ({ page }) => {
    await signIn(page, 'admin', '/admin/blob-stores')
    await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()
    await setFaults(page, [
      { match: '/blob-stores', status: 503, code: 'store_unavailable', retryAfterSecs: 1 },
    ])
    await page.reload()
    await expect(page.getByText('store_unavailable').first()).toBeVisible()
    await clearFaults(page)
    await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeEnabled({
      timeout: 10_000,
    })
    await page.getByRole('button', { name: 'Retry', exact: true }).click()
    await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/blob-stores')
      await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()
      await expectNoViolations(page, `blob store list ${theme}`)
      await page.goto('/admin/blob-stores?view=create')
      await expect(page.getByRole('heading', { name: 'Create a blob store' })).toBeVisible()
      await expectNoViolations(page, `blob store create ${theme}`)
      await page.goto('/admin/blob-stores/archive-s3')
      await expect(page.getByRole('heading', { level: 1, name: 'archive-s3' })).toBeVisible()
      await expect(page.getByRole('link', { name: 'maven-snapshots' })).toBeVisible()
      await expectNoViolations(page, `blob store detail ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/blob-stores')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      await page.goto('/admin/blob-stores')
      await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()
      expect(await hasHorizontalScroll(page), `list at ${size.name}`).toBe(false)
      await page.goto('/admin/blob-stores/archive-s3')
      await expect(page.getByRole('heading', { level: 1, name: 'archive-s3' })).toBeVisible()
      expect(await hasHorizontalScroll(page), `detail at ${size.name}`).toBe(false)
    }
  })
})

test.describe('blob store review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`blob stores ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/blob-stores')
        await expect(page.getByRole('link', { name: 'default', exact: true })).toBeVisible()
        await page.screenshot({
          path: `e2e/screenshots/blob-stores/list-${theme}-${size.name}.png`,
        })
        await page.getByRole('figure').scrollIntoViewIfNeeded()
        await page.waitForTimeout(600)
        await page.screenshot({
          path: `e2e/screenshots/blob-stores/trend-${theme}-${size.name}.png`,
        })
        await page.goto('/admin/blob-stores/archive-s3')
        await expect(page.getByRole('link', { name: 'maven-snapshots' })).toBeVisible()
        await page.screenshot({
          path: `e2e/screenshots/blob-stores/detail-${theme}-${size.name}.png`,
        })
        await page.goto('/admin/blob-stores?view=create')
        await page.getByLabel(/^Kind/).selectOption('s3')
        await page.screenshot({
          path: `e2e/screenshots/blob-stores/create-${theme}-${size.name}.png`,
        })
      })
    }
  }
})
