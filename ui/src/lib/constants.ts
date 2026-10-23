/**
 * Browser storage keys. public/theme-init.js runs before the bundle and cannot import this
 * file, so it repeats the theme and density keys as literals; theme-init.test.ts fails when
 * the two drift apart.
 */
export const STORAGE_KEYS = {
  theme: 'rustryx.theme',
  density: 'rustryx.density',
  sidebarCollapsed: 'rustryx.sidebar-collapsed',
  tableColumns: (tableId: string) => `rustryx.table.${tableId}.columns`,
  tablePageSize: (tableId: string) => `rustryx.table.${tableId}.page-size`,
} as const

export const THEME_PREFERENCES = ['light', 'dark', 'system'] as const
export type ThemePreference = (typeof THEME_PREFERENCES)[number]
export type ResolvedTheme = 'light' | 'dark'

export const DENSITIES = ['compact', 'comfortable'] as const
export type Density = (typeof DENSITIES)[number]

/** Page sizes offered by tables. The server clamps to its own maximum (1000, heavy lists 200). */
export const PAGE_SIZES = [25, 50, 100, 200] as const
export const DEFAULT_PAGE_SIZE = 50
