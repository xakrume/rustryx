import { useCallback, useState } from 'react'
import { STORAGE_KEYS } from '@/lib/constants'
import { readStorage, writeStorage } from '@/lib/storage'

/**
 * Sidebar collapsed or expanded, remembered per browser. With no stored choice it starts
 * collapsed on narrow desktops (under 1024px) so the content keeps its room, expanded above.
 */
export function useSidebarState() {
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    const stored = readStorage(STORAGE_KEYS.sidebarCollapsed)
    if (stored === 'true') return true
    if (stored === 'false') return false
    return typeof window !== 'undefined' && window.innerWidth < 1024
  })
  const toggle = useCallback(() => {
    setCollapsed((current) => {
      writeStorage(STORAGE_KEYS.sidebarCollapsed, String(!current))
      return !current
    })
  }, [])
  return { collapsed, toggle }
}
