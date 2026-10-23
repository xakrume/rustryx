import { useEffect, useState } from 'react'

/**
 * Whole seconds left until `untilMs` (epoch milliseconds), updated once a second; 0 when the
 * time has passed or when `untilMs` is null. Used for Retry-After and lock-out countdowns.
 */
export function useCountdown(untilMs: number | null): number {
  const remaining = (): number =>
    untilMs === null ? 0 : Math.max(0, Math.ceil((untilMs - Date.now()) / 1000))
  const [seconds, setSeconds] = useState(remaining)
  useEffect(() => {
    const tick = (): void =>
      setSeconds(untilMs === null ? 0 : Math.max(0, Math.ceil((untilMs - Date.now()) / 1000)))
    tick()
    if (untilMs === null) return undefined
    const id = setInterval(tick, 1000)
    return () => clearInterval(id)
  }, [untilMs])
  return seconds
}
