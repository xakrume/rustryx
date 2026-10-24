import { formatDuration } from '@/lib/format'

/** "4 d 1 h" for a node that has been up for days; shorter spans use the shared duration form. */
export function formatUptime(totalSeconds: number): string {
  const seconds = Math.max(0, Math.round(totalSeconds))
  const days = Math.floor(seconds / 86_400)
  if (days === 0) return formatDuration(seconds)
  const hours = Math.floor((seconds % 86_400) / 3_600)
  return hours === 0 ? `${days} d` : `${days} d ${hours} h`
}
