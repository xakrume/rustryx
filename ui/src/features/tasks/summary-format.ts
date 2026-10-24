import { scalarText } from '@/features/system/scalar-text'
import { formatBytes } from '@/lib/format'

/** A run counter for people: byte counts in binary units, other numbers with separators. */
export function summaryValue(key: string, value: unknown): string {
  if (typeof value === 'number' && key.includes('bytes')) return formatBytes(value)
  if (typeof value === 'number') return value.toLocaleString()
  return scalarText(value)
}
