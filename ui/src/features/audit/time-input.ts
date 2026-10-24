/** `2026-10-24T09:15:30Z` to the value of a `datetime-local` input in the local zone. */
export function isoToLocalInput(iso: string | undefined): string {
  if (iso === undefined || iso === '') return ''
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return ''
  const pad = (n: number): string => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/** The value of a `datetime-local` input (local time) to an RFC 3339 instant, or null when empty or invalid. */
export function localInputToIso(value: string): string | null {
  if (value === '') return null
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? null : date.toISOString().replace(/\.\d{3}Z$/, 'Z')
}
