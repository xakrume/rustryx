/**
 * A tiny typed event bus between the fetch layer (which sees every response) and the parts of
 * the UI that must react to what it sees: the sign-in redirect, the "server unreachable"
 * banner and the Retry-After countdown. The fetch layer must not import React or the router,
 * so it publishes here and the app subscribes once.
 */
export type ApiEvent =
  | { type: 'unauthenticated'; path: string }
  | { type: 'network-failure' }
  | { type: 'network-ok' }
  | { type: 'retry-after'; status: number; seconds: number; path: string }

type Listener = (event: ApiEvent) => void

const listeners = new Set<Listener>()

export const apiEvents = {
  subscribe(listener: Listener): () => void {
    listeners.add(listener)
    return () => listeners.delete(listener)
  },
  emit(event: ApiEvent): void {
    // Copy first: a listener may unsubscribe while handling the event.
    for (const listener of [...listeners]) {
      try {
        listener(event)
      } catch (error) {
        // One broken subscriber must not stop the others or the request that triggered it.
        queueMicrotask(() => {
          throw error
        })
      }
    }
  },
  /** Test helper: drop every subscriber. */
  reset(): void {
    listeners.clear()
  },
}
