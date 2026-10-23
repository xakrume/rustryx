import { act, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { STORAGE_KEYS } from '@/lib/constants'
import { setMedia } from '@/test/media'
import { ThemeProvider, useTheme } from './theme-provider'

function Probe() {
  const { preference, resolved, setPreference, density, setDensity } = useTheme()
  return (
    <>
      <p>
        pref={preference} resolved={resolved} density={density}
      </p>
      <button onClick={() => setPreference('dark')}>dark</button>
      <button onClick={() => setPreference('system')}>system</button>
      <button onClick={() => setDensity('comfortable')}>comfortable</button>
    </>
  )
}

const root = () => document.documentElement

describe('ThemeProvider', () => {
  it('defaults to the system theme and compact density', () => {
    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    )
    expect(screen.getByText(/pref=system resolved=light density=compact/)).toBeInTheDocument()
    expect(root()).toHaveAttribute('data-theme', 'light')
    expect(root()).toHaveAttribute('data-density', 'compact')
  })

  it('follows the operating system while the preference is system', () => {
    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    )
    act(() => setMedia((q) => q === '(prefers-color-scheme: dark)'))
    expect(root()).toHaveAttribute('data-theme', 'dark')
    act(() => setMedia(() => false))
    expect(root()).toHaveAttribute('data-theme', 'light')
  })

  it('persists an explicit choice and ignores later system changes', async () => {
    const user = userEvent.setup()
    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    )
    await user.click(screen.getByRole('button', { name: 'dark' }))
    expect(root()).toHaveAttribute('data-theme', 'dark')
    expect(window.localStorage.getItem(STORAGE_KEYS.theme)).toBe('dark')
    act(() => setMedia(() => false))
    expect(root()).toHaveAttribute('data-theme', 'dark')
    await user.click(screen.getByRole('button', { name: 'comfortable' }))
    expect(root()).toHaveAttribute('data-density', 'comfortable')
    expect(window.localStorage.getItem(STORAGE_KEYS.density)).toBe('comfortable')
  })

  it('restores the stored choice on the next visit', () => {
    window.localStorage.setItem(STORAGE_KEYS.theme, 'dark')
    window.localStorage.setItem(STORAGE_KEYS.density, 'comfortable')
    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    )
    expect(screen.getByText(/pref=dark resolved=dark density=comfortable/)).toBeInTheDocument()
  })

  it('must_not_crash_when_storage_throws', async () => {
    const boom = () => {
      throw new DOMException('denied', 'SecurityError')
    }
    const original = window.localStorage
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: { getItem: boom, setItem: boom, removeItem: boom },
    })
    try {
      const user = userEvent.setup()
      render(
        <ThemeProvider>
          <Probe />
        </ThemeProvider>,
      )
      expect(screen.getByText(/pref=system/)).toBeInTheDocument()
      await user.click(screen.getByRole('button', { name: 'dark' }))
      expect(root()).toHaveAttribute('data-theme', 'dark')
    } finally {
      Object.defineProperty(window, 'localStorage', { configurable: true, value: original })
    }
  })

  it('syncs another tab through the storage event', () => {
    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    )
    window.localStorage.setItem(STORAGE_KEYS.theme, 'dark')
    act(() => {
      window.dispatchEvent(new StorageEvent('storage', { key: STORAGE_KEYS.theme }))
    })
    expect(root()).toHaveAttribute('data-theme', 'dark')
    vi.restoreAllMocks()
  })
})
