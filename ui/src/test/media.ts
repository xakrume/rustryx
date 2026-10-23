type Matcher = (query: string) => boolean

let matcher: Matcher = () => false
const lists = new Set<{ query: string; listeners: Set<() => void>; last: boolean }>()

/**
 * jsdom has no matchMedia. This installs one whose answers a test controls: call
 * `setMedia((q) => q.includes('prefers-color-scheme: dark'))` to simulate a dark system theme;
 * every live MediaQueryList notifies its listeners, as the browser would on a change.
 */
export function setMedia(next: Matcher): void {
  matcher = next
  lists.forEach((entry) => {
    const now = matcher(entry.query)
    if (now !== entry.last) {
      entry.last = now
      entry.listeners.forEach((l) => l())
    }
  })
}

export function installMatchMedia(): void {
  window.matchMedia = (query: string): MediaQueryList => {
    const entry = { query, listeners: new Set<() => void>(), last: matcher(query) }
    lists.add(entry)
    const list = {
      get matches() {
        return matcher(query)
      },
      media: query,
      onchange: null,
      addEventListener: (_type: string, listener: () => void) => entry.listeners.add(listener),
      removeEventListener: (_type: string, listener: () => void) =>
        entry.listeners.delete(listener),
      addListener: (listener: () => void) => entry.listeners.add(listener),
      removeListener: (listener: () => void) => entry.listeners.delete(listener),
      dispatchEvent: () => false,
    }
    return list as unknown as MediaQueryList
  }
}

export function resetMedia(): void {
  matcher = () => false
  lists.clear()
}
