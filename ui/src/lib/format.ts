const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const

/**
 * Binary units (1 KiB = 1024 B), the unit blob stores and quotas count in. Values below
 * 1024 print as whole bytes; above that one decimal, dropped when it is zero.
 */
export function formatBytes(bytes: number | null | undefined, locale?: string): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes < 0) return '-'
  if (bytes < 1024) return `${new Intl.NumberFormat(locale).format(Math.round(bytes))} B`
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  const text = new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(value)
  return `${text} ${BYTE_UNITS[unit] ?? 'B'}`
}

/** Exact byte count with thousands separators, for tooltips ("8,123,456,789 bytes"). */
export function formatExactBytes(bytes: number, locale?: string): string {
  return `${new Intl.NumberFormat(locale).format(bytes)} bytes`
}

const RELATIVE_STEPS: readonly { unit: Intl.RelativeTimeFormatUnit; seconds: number }[] = [
  { unit: 'year', seconds: 365 * 24 * 3600 },
  { unit: 'month', seconds: 30 * 24 * 3600 },
  { unit: 'week', seconds: 7 * 24 * 3600 },
  { unit: 'day', seconds: 24 * 3600 },
  { unit: 'hour', seconds: 3600 },
  { unit: 'minute', seconds: 60 },
]

/** "5 minutes ago", "in 2 days", "just now" (under 10 seconds either way). */
export function formatRelative(date: Date, now: Date = new Date(), locale?: string): string {
  const diffSeconds = Math.round((date.getTime() - now.getTime()) / 1000)
  const abs = Math.abs(diffSeconds)
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })
  if (abs < 10) return rtf.format(0, 'second')
  for (const step of RELATIVE_STEPS) {
    if (abs >= step.seconds) return rtf.format(Math.round(diffSeconds / step.seconds), step.unit)
  }
  return rtf.format(diffSeconds, 'second')
}

/** Local date and time for display, for example "24 Oct 2026, 09:15:30". */
export function formatDateTime(date: Date, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'medium' }).format(date)
}

/** "1 h 5 min", "45 s": a duration in whole seconds, as counters and Retry-After show it. */
export function formatDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds))
  if (s < 60) return `${s} s`
  const minutes = Math.floor(s / 60)
  const seconds = s % 60
  if (minutes < 60) return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`
}

/** Parse an ISO string from the API; null for missing or invalid values. */
export function parseDate(value: string | null | undefined): Date | null {
  if (!value) return null
  const d = new Date(value)
  return Number.isNaN(d.getTime()) ? null : d
}
