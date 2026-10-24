import { t } from '@/i18n'
import { formatDateTime, parseDate } from '@/lib/format'
import type { components } from '@/api/schema'

export type Schedule = components['schemas']['Schedule']

/** A schedule in words. Times are UTC in the contract; the absolute time is shown local. */
export function describeSchedule(schedule: Schedule | null): string {
  if (schedule === null) return t('tasks.schedule.manual')
  switch (schedule.type) {
    case 'manual':
      return t('tasks.schedule.manual')
    case 'once': {
      const at = parseDate(schedule.at)
      return t('tasks.schedule.once', { time: at ? formatDateTime(at) : '-' })
    }
    case 'hourly':
      return t('tasks.schedule.hourly')
    case 'daily':
      return t('tasks.schedule.daily')
    case 'weekly':
      return t('tasks.schedule.weekly')
    case 'monthly':
      return t('tasks.schedule.monthly')
    case 'cron':
      return t('tasks.schedule.cron', { expression: schedule.cron ?? '-' })
    default:
      return t('tasks.schedule.unknown')
  }
}
