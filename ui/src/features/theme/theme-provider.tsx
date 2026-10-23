import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { useMediaQuery } from '@/hooks/use-media-query'
import {
  DENSITIES,
  STORAGE_KEYS,
  THEME_PREFERENCES,
  type Density,
  type ResolvedTheme,
  type ThemePreference,
} from '@/lib/constants'
import { readStorage, writeStorage } from '@/lib/storage'

interface ThemeContextValue {
  /** What the user chose: light, dark or system. */
  preference: ThemePreference
  /** What is applied: system is resolved to light or dark. */
  resolved: ResolvedTheme
  setPreference: (preference: ThemePreference) => void
  density: Density
  setDensity: (density: Density) => void
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

function readPreference(): ThemePreference {
  const stored = readStorage(STORAGE_KEYS.theme)
  return THEME_PREFERENCES.find((p) => p === stored) ?? 'system'
}

function readDensity(): Density {
  const stored = readStorage(STORAGE_KEYS.density)
  return DENSITIES.find((d) => d === stored) ?? 'compact'
}

/**
 * Applies the theme and density to <html> as attributes (`data-theme` is always the resolved
 * light or dark, see styles/tokens.css) and keeps them in localStorage. The first paint is
 * already correct because public/theme-init.js sets the same attributes from the same keys;
 * this provider takes over from there and reacts to later changes: a menu click, another tab
 * (storage event), or the operating system switching while the preference is "system".
 */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [preference, setPreferenceState] = useState<ThemePreference>(readPreference)
  const [density, setDensityState] = useState<Density>(readDensity)
  const systemDark = useMediaQuery('(prefers-color-scheme: dark)')
  const resolved: ResolvedTheme =
    preference === 'system' ? (systemDark ? 'dark' : 'light') : preference

  // Layout effects: the attributes change before the browser paints and before the passive
  // effects of descendants run, so a component that reads computed colors (charts) sees the
  // new theme, and the screen never shows one frame of the old one.
  useLayoutEffect(() => {
    document.documentElement.setAttribute('data-theme', resolved)
  }, [resolved])

  useLayoutEffect(() => {
    document.documentElement.setAttribute('data-density', density)
  }, [density])

  useEffect(() => {
    const onStorage = (event: StorageEvent): void => {
      if (event.key === STORAGE_KEYS.theme) setPreferenceState(readPreference())
      if (event.key === STORAGE_KEYS.density) setDensityState(readDensity())
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])

  const setPreference = useCallback((next: ThemePreference) => {
    setPreferenceState(next)
    writeStorage(STORAGE_KEYS.theme, next)
  }, [])

  const setDensity = useCallback((next: Density) => {
    setDensityState(next)
    writeStorage(STORAGE_KEYS.density, next)
  }, [])

  const value = useMemo<ThemeContextValue>(
    () => ({ preference, resolved, setPreference, density, setDensity }),
    [preference, resolved, setPreference, density, setDensity],
  )
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext)
  if (ctx === null) throw new Error('useTheme must be used inside <ThemeProvider>')
  return ctx
}
