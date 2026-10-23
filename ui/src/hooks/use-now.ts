import { useSyncExternalStore } from 'react'

/**
 * One shared ticker for every relative timestamp on the page: a table with 200 rows needs one
 * interval, not 200. The snapshot is the current minute, so subscribers re-render once a minute.
 */
const listeners = new Set<() => void>()
let timer: ReturnType<typeof setInterval> | null = null

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  timer ??= setInterval(() => listeners.forEach((l) => l()), 30_000)
  return () => {
    listeners.delete(listener)
    if (listeners.size === 0 && timer !== null) {
      clearInterval(timer)
      timer = null
    }
  }
}

const minute = (): number => Math.floor(Date.now() / 60_000)

export function useNow(): Date {
  const bucket = useSyncExternalStore(subscribe, minute, minute)
  return new Date(bucket * 60_000)
}
