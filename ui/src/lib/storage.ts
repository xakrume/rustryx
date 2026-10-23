/**
 * localStorage that never throws. It can be missing or throw on access (private windows,
 * blocked site data, previews, thumbnail capture), and a UI preference is never worth a
 * crash, so every call degrades to "not stored". Only per-viewer conveniences go here:
 * never tokens, never anything that has to survive or be shared.
 */
export function readStorage(key: string): string | null {
  try {
    return window.localStorage.getItem(key)
  } catch {
    return null
  }
}

export function writeStorage(key: string, value: string): boolean {
  try {
    window.localStorage.setItem(key, value)
    return true
  } catch {
    return false
  }
}

export function removeStorage(key: string): void {
  try {
    window.localStorage.removeItem(key)
  } catch {
    // Nothing stored, nothing to remove.
  }
}

/** Read a JSON value; malformed or foreign data yields the fallback, never an exception. */
export function readJson<T>(key: string, guard: (value: unknown) => value is T, fallback: T): T {
  const raw = readStorage(key)
  if (raw === null) return fallback
  try {
    const parsed: unknown = JSON.parse(raw)
    return guard(parsed) ? parsed : fallback
  } catch {
    return fallback
  }
}

export function writeJson(key: string, value: unknown): boolean {
  return writeStorage(key, JSON.stringify(value))
}
