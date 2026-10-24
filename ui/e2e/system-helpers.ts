import AxeBuilder from '@axe-core/playwright'
import { expect, type Page } from '@playwright/test'

/** axe in a real browser, so color contrast is measured on the real rendering of both themes. */
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

export interface MockFault {
  match: string
  status: number
  code: string
  retryAfterSecs?: number
  method?: string
}

/** Error states of the in-browser mock (`features/system/mock-kit.ts`): faults live in sessionStorage. */
export async function setFaults(page: Page, faults: readonly MockFault[]): Promise<void> {
  await page.evaluate((value) => {
    window.sessionStorage.setItem('ryx-mock-faults', JSON.stringify(value))
  }, faults)
}

export async function clearFaults(page: Page): Promise<void> {
  await page.evaluate(() => window.sessionStorage.removeItem('ryx-mock-faults'))
}

/** `healthy` (default), `mixed`, `dependencies-down`, `not-ready`: see `features/system/mocks.ts`. */
export async function setStatusScenario(page: Page, name: string): Promise<void> {
  await page.evaluate((value) => window.sessionStorage.setItem('ryx-mock-status', value), name)
}

export const WIDTHS = [
  { name: '375', width: 375, height: 760 },
  { name: '768', width: 768, height: 800 },
  { name: '1440', width: 1440, height: 800 },
] as const

/** Press Tab until the element has focus (a keyboard user's way to a control); gives up after 80 presses. */
export async function tabTo(page: Page, name: RegExp | string, role = 'button'): Promise<void> {
  const target = page.getByRole(role as 'button', { name })
  for (let i = 0; i < 80; i += 1) {
    if (await target.evaluate((el) => el === document.activeElement).catch(() => false)) return
    await page.keyboard.press('Tab')
  }
  throw new Error(`Tab never reached ${String(name)}`)
}
