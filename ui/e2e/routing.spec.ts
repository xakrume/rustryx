import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'
import { expectNoViolations, tabTo, WIDTHS } from './system-helpers'

const BLOCK_INTERNAL = '0192f6c4-5a1e-7000-8009-000000000001'

test.describe('routing rules', () => {
  test('creates a rule by keyboard, adds a pattern and tests paths before saving', async ({
    page,
  }) => {
    await signIn(page, 'admin', '/admin/routing-rules')
    await expect(
      page.getByRole('link', { name: 'block-internal-paths', exact: true }),
    ).toBeVisible()

    await tabTo(page, 'Create rule')
    await page.keyboard.press('Enter')
    await expect(
      page.getByRole('heading', { level: 1, name: 'Create a routing rule' }),
    ).toBeVisible()

    await page.getByLabel(/^Name/).focus()
    await page.keyboard.type('e2e-block-tmp')
    await page.getByLabel('Pattern 1', { exact: true }).focus()
    await page.keyboard.type('^/tmp/.*')
    await tabTo(page, 'Add pattern')
    await page.keyboard.press('Enter')
    // The new row has the focus: keep typing.
    await expect(page.getByLabel('Pattern 2', { exact: true })).toBeFocused()
    await page.keyboard.type('.*\\.exe$')
    await expect(page.getByText('All patterns are valid.')).toBeVisible()

    await page.getByLabel('Paths to test').fill('/tmp/cache.bin\n/lib/tool.exe\n/lib/tool.jar')
    await tabTo(page, 'Test paths')
    await page.keyboard.press('Enter')
    const results = page.getByRole('region', { name: 'Test results' })
    await expect(results.getByText('Blocked')).toHaveCount(2)
    await expect(results.getByText('Allowed')).toHaveCount(1)
    await expect(results.getByText('^/tmp/.*')).toBeVisible()

    await page.getByRole('button', { name: 'Save rule' }).click()
    await expect(page.getByText('Rule e2e-block-tmp created', { exact: true })).toBeVisible()
    await expect(
      page.getByRole('heading', { level: 1, name: 'Edit routing rule e2e-block-tmp' }),
    ).toBeVisible()
  })

  test('shows the position of a pattern the engine cannot run', async ({ page }) => {
    await signIn(page, 'admin', '/admin/routing-rules?edit=new')
    await page.getByLabel('Pattern 1', { exact: true }).fill('(a)\\1')
    await expect(
      page.getByText(/Position 3: Backreferences and lookaround are not supported/),
    ).toBeVisible()
    await expect(page.getByLabel('Pattern 1', { exact: true })).toHaveAttribute(
      'aria-invalid',
      'true',
    )
  })

  test('must_block_deleting_a_rule_in_use_and_name_the_repositories', async ({ page }) => {
    await signIn(page, 'admin', `/admin/routing-rules?edit=${BLOCK_INTERNAL}`)
    await expect(page.getByRole('link', { name: 'maven-proxy' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Delete', exact: true })).toBeDisabled()
    await expect(page.getByText('This rule is in use')).toBeVisible()
  })

  for (const theme of ['light', 'dark'] as const) {
    test(`pages pass axe in the ${theme} theme`, async ({ page }) => {
      await preferTheme(page, theme)
      await signIn(page, 'admin', '/admin/routing-rules')
      await expect(
        page.getByRole('link', { name: 'block-internal-paths', exact: true }),
      ).toBeVisible()
      await expectNoViolations(page, `routing list ${theme}`)
      await page.goto(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`)
      await page.getByLabel('Paths to test').fill('/internal/a.jar\n/public/b.jar')
      await page.getByRole('button', { name: 'Test paths' }).click()
      await expect(page.getByRole('region', { name: 'Test results' })).toBeVisible()
      await expectNoViolations(page, `routing editor ${theme}`)
    })
  }

  test('has no horizontal scroll at phone, tablet and desktop widths', async ({ page }) => {
    await signIn(page, 'admin', '/admin/routing-rules')
    for (const size of WIDTHS) {
      await page.setViewportSize({ width: size.width, height: size.height })
      await page.goto('/admin/routing-rules')
      await expect(
        page.getByRole('link', { name: 'block-internal-paths', exact: true }),
      ).toBeVisible()
      expect(await hasHorizontalScroll(page), `list at ${size.name}`).toBe(false)
      await page.goto(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`)
      await expect(page.getByLabel('Pattern 1', { exact: true })).toBeVisible()
      expect(await hasHorizontalScroll(page), `editor at ${size.name}`).toBe(false)
    }
  })
})

test.describe('routing review screenshots', () => {
  test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
  for (const theme of ['light', 'dark'] as const) {
    for (const size of WIDTHS) {
      test(`routing ${theme} ${size.name}`, async ({ page }) => {
        await preferTheme(page, theme)
        await page.setViewportSize({ width: size.width, height: size.height })
        await signIn(page, 'admin', '/admin/routing-rules')
        await expect(
          page.getByRole('link', { name: 'block-internal-paths', exact: true }),
        ).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/routing/list-${theme}-${size.name}.png` })
        await page.goto(`/admin/routing-rules?edit=${BLOCK_INTERNAL}`)
        await page.getByLabel('Paths to test').fill('/internal/a.jar\n/public/b.jar')
        await page.getByRole('button', { name: 'Test paths' }).click()
        await expect(page.getByRole('region', { name: 'Test results' })).toBeVisible()
        await page.screenshot({ path: `e2e/screenshots/routing/editor-${theme}-${size.name}.png` })
        await page.getByRole('region', { name: 'Test results' }).scrollIntoViewIfNeeded()
        await page.screenshot({ path: `e2e/screenshots/routing/tester-${theme}-${size.name}.png` })
      })
    }
  }
})
