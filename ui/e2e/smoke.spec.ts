import { expect, hasHorizontalScroll, signIn, test } from './fixtures'

test.describe('shell smoke test (mock backend, production CSP)', () => {
  test('signs in with the mock, navigates the shell and signs out', async ({ page }) => {
    await signIn(page, 'admin')
    await expect(page).toHaveURL('/')
    await expect(page.getByRole('heading', { level: 1, name: 'Home' })).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Storage' }).last()).toBeVisible()

    const nav = page.getByRole('navigation', { name: 'Main navigation' })
    await nav.getByRole('link', { name: 'Repositories' }).click()
    await expect(page).toHaveURL('/repositories')
    await expect(page.getByRole('heading', { level: 1, name: 'Repositories' })).toBeFocused()

    // Administration groups are collapsed until opened.
    await nav.getByRole('button', { name: /Identity and access/ }).click()
    await nav.getByRole('link', { name: 'Users' }).click()
    await expect(page).toHaveURL('/admin/users')
    const trail = page.getByRole('navigation', { name: 'Breadcrumb' })
    await expect(trail).toContainText('Administration')
    await expect(trail.getByText('Users')).toHaveAttribute('aria-current', 'page')

    await page.getByRole('button', { name: /Account menu/ }).click()
    await expect(page.getByRole('menu')).toContainText('nx-admin')
    await page.getByRole('menuitem', { name: 'Sign out' }).click()
    await expect(page).toHaveURL(/\/login/)
    await expect(page.getByRole('heading', { name: 'Sign in to Rustryx' })).toBeVisible()
  })

  test('shows the generic message for a wrong password', async ({ page }) => {
    await page.goto('/login')
    await page.getByLabel(/^Username/).fill('admin')
    await page.getByLabel(/^Password/).fill('definitely-wrong')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByText('The username or password is not correct.')).toBeVisible()
    await expect(page).toHaveURL(/\/login/)
  })

  test('protects an admin page: login first, then straight to it', async ({ page }) => {
    await page.goto('/admin/users')
    await expect(page).toHaveURL(/\/login\?return_to=%2Fadmin%2Fusers/)
    await page.getByLabel(/^Username/).fill('admin')
    await page.getByLabel(/^Password/).fill('mock-password')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page).toHaveURL('/admin/users')
    await expect(page.getByRole('heading', { level: 1, name: 'Users' })).toBeVisible()
  })

  test('a developer sees no administration and gets a forbidden page by address', async ({
    page,
  }) => {
    // The mock lets only the local admin sign in with a password; alice is an SSO account,
    // so her session is created through the mock API, as the SSO callback would.
    await page.goto('/login')
    await page.evaluate(() => {
      window.sessionStorage.setItem(
        'ryx-mock-session',
        JSON.stringify({
          username: 'alice',
          csrf: 'c',
          issuedAt: Date.now(),
          touchedAt: Date.now(),
        }),
      )
    })
    await page.goto('/admin/users')
    await expect(page.getByRole('heading', { name: 'You do not have access' })).toBeVisible()
    await expect(page.getByText(/Missing privilege/)).toBeVisible()
    await expect(
      page
        .getByRole('navigation', { name: 'Main navigation' })
        .getByRole('button', { name: /Identity/ }),
    ).toHaveCount(0)
  })

  test('toggles the theme and remembers it after a reload', async ({ page }) => {
    await signIn(page)
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    await page.getByRole('button', { name: /^Theme:/ }).click()
    await page.getByRole('menuitemradio', { name: 'Dark' }).click()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor)
    expect(background).toBe('rgb(15, 23, 42)')
    await page.reload()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  })

  test('follows the system theme before the bundle runs (no flash)', async ({ browser }) => {
    const context = await browser.newContext({ colorScheme: 'dark' })
    const page = await context.newPage()
    await page.goto('/login')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    await context.close()
  })

  test('switches density', async ({ page }) => {
    await signIn(page)
    await page.getByRole('button', { name: /^Density:/ }).click()
    await page.getByRole('menuitemradio', { name: 'Comfortable' }).click()
    await expect(page.locator('html')).toHaveAttribute('data-density', 'comfortable')
    const height = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--control-h-base').trim(),
    )
    expect(height).toBe('2.5rem')
  })

  test('opens the command palette with Ctrl+K, finds a page and jumps there', async ({ page }) => {
    await signIn(page, 'admin')
    await page.keyboard.press('Control+K')
    const dialog = page.getByRole('dialog', { name: 'Command palette' })
    await expect(dialog).toBeVisible()
    await dialog.getByRole('combobox').fill('audit')
    await page.keyboard.press('Enter')
    await expect(page).toHaveURL('/admin/audit')
    await expect(page.getByRole('dialog')).toHaveCount(0)
    await page.keyboard.press('Control+K')
    await expect(dialog).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(dialog).toHaveCount(0)
  })

  test('works with the keyboard only: skip link, sidebar arrows, focus after navigation', async ({
    page,
  }) => {
    await signIn(page, 'admin')
    // A fresh load: focus starts at the top of the document, the way it does for a real visit.
    await page.goto('/')
    await expect(page.getByRole('main')).toBeVisible()
    await page.keyboard.press('Tab')
    await expect(page.getByRole('link', { name: 'Skip to main content' })).toBeFocused()
    await page.keyboard.press('Tab')
    const nav = page.getByRole('navigation', { name: 'Main navigation' })
    await expect(nav.getByRole('link', { name: 'Home' })).toBeFocused()
    await page.keyboard.press('ArrowDown')
    await expect(nav.getByRole('link', { name: 'Repositories' })).toBeFocused()
    await page.keyboard.press('ArrowDown')
    await expect(nav.getByRole('link', { name: 'Search' })).toBeFocused()
    await page.keyboard.press('Enter')
    await expect(page).toHaveURL('/search')
    // Focus moved to the new page's heading, so the next Tab continues inside the page.
    await expect(page.getByRole('heading', { level: 1, name: 'Search' })).toBeFocused()
    await page.keyboard.press('End')
    await page.keyboard.press('Home')
    // The skip link works too.
    await page.keyboard.press('Shift+Tab')
    await page.keyboard.press('Shift+Tab')
  })

  test('collapses the sidebar to an icon rail and remembers it', async ({ page }) => {
    await signIn(page)
    await page.getByRole('button', { name: 'Collapse sidebar' }).click()
    await expect(page.getByRole('button', { name: 'Expand sidebar' })).toBeVisible()
    await page.reload()
    await expect(page.getByRole('button', { name: 'Expand sidebar' })).toBeVisible()
  })

  test('shows the 404 page for an unknown address', async ({ page }) => {
    await page.goto('/definitely/not/here')
    await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible()
  })

  test.describe('touch phone', () => {
    // isMobile and hasTouch make Chromium report a coarse pointer, which is what the 44px
    // touch target rule keys on (styles/tokens.css).
    test.use({ viewport: { width: 375, height: 812 }, hasTouch: true, isMobile: true })

    test('is usable at 375px: drawer navigation, 44px targets, no horizontal scroll', async ({
      page,
    }) => {
      await signIn(page)
      expect(await hasHorizontalScroll(page)).toBe(false)
      await expect(page.getByRole('button', { name: 'Collapse sidebar' })).toBeHidden()
      await page.getByRole('button', { name: 'Open navigation menu' }).click()
      const drawer = page.getByRole('dialog')
      await expect(drawer).toBeVisible()
      await drawer.getByRole('link', { name: 'Search' }).click()
      await expect(page).toHaveURL('/search')
      await expect(drawer).toHaveCount(0)
      expect(await hasHorizontalScroll(page)).toBe(false)
      // Touch targets are at least 44px high.
      const box = await page.getByRole('button', { name: 'Open navigation menu' }).boundingBox()
      expect(box?.height ?? 0).toBeGreaterThanOrEqual(44)
    })
  })

  test('gallery: no horizontal page scroll at 375, 768 and 1440 in both themes', async ({
    page,
  }) => {
    await signIn(page)
    for (const theme of ['light', 'dark'] as const) {
      await page.evaluate((value) => window.localStorage.setItem('rustryx.theme', value), theme)
      for (const width of [375, 768, 1440]) {
        await page.setViewportSize({ width, height: 900 })
        await page.goto('/__components')
        await expect(
          page.getByRole('heading', { level: 1, name: 'Component gallery' }),
        ).toBeVisible()
        await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toBeVisible()
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
        expect(await hasHorizontalScroll(page), `${theme} ${width}px`).toBe(false)
      }
    }
  })
})
