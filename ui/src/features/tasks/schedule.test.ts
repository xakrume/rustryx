import { describe, expect, it } from 'vitest'
import { describeSchedule } from './schedule'
import { summaryValue } from './summary-format'

describe('describeSchedule', () => {
  it('describes every kind of schedule in words', () => {
    expect(describeSchedule(null)).toBe('Manual only')
    expect(describeSchedule({ type: 'manual' })).toBe('Manual only')
    expect(describeSchedule({ type: 'hourly' })).toBe('Every hour')
    expect(describeSchedule({ type: 'daily' })).toBe('Every day')
    expect(describeSchedule({ type: 'weekly' })).toBe('Every week')
    expect(describeSchedule({ type: 'monthly' })).toBe('Every month')
    expect(describeSchedule({ type: 'cron', cron: '0 2 * * *' })).toBe('Cron 0 2 * * * (UTC)')
    expect(describeSchedule({ type: 'cron', cron: null })).toBe('Cron - (UTC)')
    expect(describeSchedule({ type: 'once', at: null })).toBe('Once at -')
    expect(describeSchedule({ type: 'once', at: '2026-10-24T09:15:30Z' })).toMatch(/^Once at /)
  })

  it('tolerates a schedule type from a newer server', () => {
    expect(describeSchedule({ type: 'fortnightly' } as never)).toBe('Unknown schedule')
  })
})

describe('summaryValue', () => {
  it('shows byte counts in binary units and other numbers with separators', () => {
    expect(summaryValue('bytes_reclaimed', 1_073_741_824)).toBe('1 GiB')
    expect(summaryValue('blobs_removed', 1204)).toBe((1204).toLocaleString())
    expect(summaryValue('mode', 'full')).toBe('full')
    expect(summaryValue('detail', { a: 1 })).toBe('{"a":1}')
  })
})
