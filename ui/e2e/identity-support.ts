import AxeBuilder from '@axe-core/playwright'
import type { Locator, Page } from '@playwright/test'
import { expect, hasHorizontalScroll, preferTheme, signIn, test } from './fixtures'

/** Helpers of the identity and security specs (users, roles, tokens, audit and the rest). */

/** axe in the real browser, so contrast is measured on the real rendering of the current theme. */
export async function expectNoViolations(page: Page, label: string): Promise<void> {
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'])
    .analyze()
  const summary = results.violations.map(
    (v) =>
      `${v.id} (${v.impact}): ${v.nodes
        .map((n) => n.target.join(' '))
        .slice(0, 3)
        .join(' | ')}`,
  )
  expect(summary, label).toEqual([])
}

/** Keyboard only: focus the control and press a key on it, no pointer involved. */
export async function activate(page: Page, target: Locator, key = 'Enter'): Promise<void> {
  await target.focus()
  await page.keyboard.press(key)
}

/**
 * The File System Access API opens a native save dialog that a headless browser cannot answer.
 * Removing it makes the export use the in-memory download, which is what browsers without the API do.
 */
export async function withoutFilePicker(page: Page): Promise<void> {
  await page.addInitScript(() => {
    delete (window as { showSaveFilePicker?: unknown }).showSaveFilePicker
  })
}

export const SIZES = [
  { name: '375', width: 375, height: 812 },
  { name: '768', width: 768, height: 900 },
  { name: '1440', width: 1440, height: 900 },
] as const

export interface ShotPage {
  /** File name part, for example `list` or `editor`. */
  name: string
  path: string
  /** Waits until the page is ready to be photographed. */
  ready: (page: Page) => Promise<void>
}

/**
 * Review screenshots of a feature at 375, 768 and 1440 px in both themes, to
 * `e2e/screenshots/<feature>/`. Off by default so a normal run rewrites no committed file:
 * `SCREENSHOTS=1 npm run test:e2e -- <feature>`. Viewport size only, so the files stay small. Every
 * photo also checks that the page does not scroll sideways at that width.
 */
export function screenshotSuite(feature: string, pages: readonly ShotPage[], user = 'admin'): void {
  test.describe(`${feature} screenshots`, () => {
    test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to refresh the review screenshots')
    for (const theme of ['light', 'dark'] as const) {
      for (const size of SIZES) {
        for (const shot of pages) {
          test(`${shot.name} ${theme} ${size.name}`, async ({ page }) => {
            await preferTheme(page, theme)
            await page.setViewportSize({ width: size.width, height: size.height })
            if (shot.path.startsWith('/login')) await page.goto(shot.path)
            else await signIn(page, user, shot.path)
            await shot.ready(page)
            expect(await hasHorizontalScroll(page)).toBe(false)
            await page.screenshot({
              path: `e2e/screenshots/${feature}/${shot.name}-${theme}-${size.name}.png`,
            })
          })
        }
      }
    }
  })
}

export { expect, hasHorizontalScroll, preferTheme, signIn, test }
