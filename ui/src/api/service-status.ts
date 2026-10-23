import { useSyncExternalStore } from 'react'
import { apiEvents } from './events'

/**
 * What the UI knows about the reachability of the server, fed by the fetch layer through the
 * event bus: no HTTP answer at all (backend down), or an answer that said "wait" (429, 503
 * with Retry-After). The banners read it; query retries do the actual waiting.
 */
export interface ServiceState {
  backendDown: boolean
  /** Epoch ms until which the server asked clients to hold back, or null. */
  retryUntil: number | null
  retryStatus: number | null
}

const INITIAL: ServiceState = { backendDown: false, retryUntil: null, retryStatus: null }

let state: ServiceState = INITIAL
const listeners = new Set<() => void>()

function set(next: ServiceState): void {
  if (
    next.backendDown === state.backendDown &&
    next.retryUntil === state.retryUntil &&
    next.retryStatus === state.retryStatus
  ) {
    return
  }
  state = next
  listeners.forEach((l) => l())
}

/** Subscribe the store to the event bus. Call once at startup; returns the unsubscribe. */
export function installServiceStatus(): () => void {
  return apiEvents.subscribe((event) => {
    switch (event.type) {
      case 'network-failure':
        set({ ...state, backendDown: true })
        break
      case 'network-ok':
        // Any HTTP answer proves the server is reachable again.
        set({ ...state, backendDown: false })
        break
      case 'retry-after':
        set({
          ...state,
          retryUntil: Date.now() + event.seconds * 1000,
          retryStatus: event.status,
        })
        break
      default:
        break
    }
  })
}

// Arrow properties, not methods: they are handed to useSyncExternalStore without `this`.
export const serviceStatus = {
  subscribe: (listener: () => void): (() => void) => {
    listeners.add(listener)
    return () => listeners.delete(listener)
  },
  getSnapshot: (): ServiceState => state,
  /** The wait is over (or the user chose to retry now). */
  clearRetry: (): void => {
    set({ ...state, retryUntil: null, retryStatus: null })
  },
  reset: (): void => {
    state = INITIAL
    listeners.forEach((l) => l())
  },
}

export function useServiceState(): ServiceState {
  return useSyncExternalStore(
    serviceStatus.subscribe,
    serviceStatus.getSnapshot,
    serviceStatus.getSnapshot,
  )
}
