import { useSyncExternalStore } from 'react'

function subscribe(notify: () => void): () => void {
  const observer = new MutationObserver(notify)
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  return () => observer.disconnect()
}

const currentTheme = (): string => document.documentElement.getAttribute('data-theme') ?? 'light'

/**
 * useSyncExternalStore needs a snapshot that is the SAME array until something changed, so the
 * resolved colors are cached per theme and token list (a handful of entries at most).
 */
const cache = new Map<string, readonly string[]>()

function read(key: string): readonly string[] {
  const cacheKey = `${currentTheme()}#${key}`
  const hit = cache.get(cacheKey)
  if (hit) return hit
  const style = getComputedStyle(document.documentElement)
  const colors = key.split('|').map((name) => style.getPropertyValue(name).trim() || 'currentColor')
  cache.set(cacheKey, colors)
  return colors
}

/**
 * The computed values of CSS custom properties, as concrete colors, re-read when the theme
 * changes. SVG presentation attributes (`stroke="..."`, `fill="..."`) do NOT understand
 * `var(--color-x)`: such a value is invalid and silently falls back to black. Libraries that
 * draw SVG from props (Recharts) therefore need the resolved color. The tokens in
 * styles/tokens.css stay the single source; this only reads them.
 *
 * Observes the data-theme attribute directly (not the ThemeProvider), so it is right however
 * the attribute was set: the menu, the system preference, another tab, public/theme-init.js.
 * Where nothing is resolvable (a test without the stylesheet) the value is `currentColor`.
 */
export function useCssColors(names: readonly string[]): readonly string[] {
  const key = names.join('|')
  return useSyncExternalStore(
    subscribe,
    () => read(key),
    () => names.map(() => 'currentColor'),
  )
}
