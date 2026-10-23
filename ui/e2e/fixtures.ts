import { expect, test as base, type Page } from '@playwright/test'

declare global {
  interface Window {
    __cspViolations?: string[]
  }
}

/**
 * Every test also fails on a Content-Security-Policy violation, an uncaught page error or a
 * console error. The preview server sends the real production policy (scripts/csp.mjs), so a
 * library that injects a <style> element or an inline script is caught here, not in a
 * customer's browser. "Failed to load resource" lines are the browser reporting 4xx answers
 * the mock sends on purpose (wrong password, expired session).
 */
export const test = base.extend<{ problems: string[] }>({
  problems: [
    async ({ page }, use) => {
      const problems: string[] = []
      page.on('console', (message) => {
        if (message.type() === 'error' && !message.text().startsWith('Failed to load resource')) {
          problems.push(`console error: ${message.text()}`)
        }
      })
      page.on('pageerror', (error) => problems.push(`page error: ${error.message}`))
      await page.addInitScript(() => {
        document.addEventListener('securitypolicyviolation', (event) => {
          window.__cspViolations ??= []
          window.__cspViolations.push(`${event.violatedDirective}: ${event.blockedURI}`)
        })
      })
      await use(problems)
      const violations = await page.evaluate(() => window.__cspViolations ?? []).catch(() => [])
      expect([...problems, ...violations.map((v) => `CSP violation ${v}`)]).toEqual([])
    },
    { auto: true },
  ],
})

export { expect }

export const MOCK_PASSWORD = 'mock-password'

/** Sign in through the real login form against the in-browser mock. */
export async function signIn(page: Page, username = 'admin', returnTo?: string): Promise<void> {
  await page.goto(returnTo ? `/login?return_to=${encodeURIComponent(returnTo)}` : '/login')
  await page.getByLabel(/^Username/).fill(username)
  await page.getByLabel(/^Password/).fill(MOCK_PASSWORD)
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.waitForURL((url) => !url.pathname.startsWith('/login'))
  // The shell mounts after /me and the session arrived. The route focus manager moves focus to
  // the page heading once the new page is in place, which also means the shell's effects (the
  // Ctrl+K listener) are attached: keys pressed any earlier would be lost.
  await page.getByRole('main').waitFor()
  await page.waitForFunction(() => document.activeElement?.hasAttribute('data-page-title') === true)
}

/** Start in a given theme without touching the UI: the same storage the app reads. */
export async function preferTheme(page: Page, theme: 'light' | 'dark'): Promise<void> {
  await page.addInitScript((value) => {
    window.localStorage.setItem('rustryx.theme', value)
  }, theme)
}

/**
 * True when the page or the main scroll area can scroll sideways. The shell scrolls inside
 * <main>, so the document alone would never report an overflowing wide table or code block.
 */
export async function hasHorizontalScroll(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const main = document.querySelector('main')
    const pageOverflow = document.documentElement.scrollWidth > window.innerWidth
    const mainOverflow = main ? main.scrollWidth > main.clientWidth : false
    return pageOverflow || mainOverflow
  })
}
