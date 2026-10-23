import { useEffect, useState } from 'react'

/**
 * True only after `flag` has been true for `delayMs` without a break. Loading skeletons use it
 * with 150 ms so a fast response never flashes a skeleton (docs/ui-page-inventory.md, section 5).
 */
export function useDelayedFlag(flag: boolean, delayMs: number): boolean {
  const [delayed, setDelayed] = useState(false)
  useEffect(() => {
    if (!flag) {
      setDelayed(false)
      return undefined
    }
    const id = setTimeout(() => setDelayed(true), delayMs)
    return () => clearTimeout(id)
  }, [flag, delayMs])
  return flag && delayed
}
