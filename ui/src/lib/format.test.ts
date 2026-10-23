import { describe, expect, it } from 'vitest'
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  formatExactBytes,
  formatRelative,
  parseDate,
} from './format'

describe('formatBytes', () => {
  it('uses binary units and drops a zero decimal', () => {
    expect(formatBytes(0, 'en')).toBe('0 B')
    expect(formatBytes(1023, 'en')).toBe('1,023 B')
    expect(formatBytes(1024, 'en')).toBe('1 KiB')
    expect(formatBytes(1536, 'en')).toBe('1.5 KiB')
    expect(formatBytes(8_123_456_789, 'en')).toBe('7.6 GiB')
    expect(formatBytes(1024 ** 5 * 3, 'en')).toBe('3 PiB')
  })

  it('shows a dash for unknown or invalid values', () => {
    for (const value of [null, undefined, Number.NaN, -1, Number.POSITIVE_INFINITY]) {
      expect(formatBytes(value)).toBe('-')
    }
  })

  it('gives the exact count for tooltips', () => {
    expect(formatExactBytes(8_123_456_789, 'en')).toBe('8,123,456,789 bytes')
  })
})

describe('formatRelative', () => {
  const now = new Date('2026-10-24T12:00:00Z')
  it('speaks in the largest fitting unit', () => {
    expect(formatRelative(new Date('2026-10-24T11:55:00Z'), now, 'en')).toBe('5 minutes ago')
    expect(formatRelative(new Date('2026-10-24T09:00:00Z'), now, 'en')).toBe('3 hours ago')
    expect(formatRelative(new Date('2026-10-26T12:00:00Z'), now, 'en')).toBe('in 2 days')
  })
  it('says now for a few seconds either way', () => {
    expect(formatRelative(new Date('2026-10-24T11:59:55Z'), now, 'en')).toBe('now')
  })
})

describe('formatDuration', () => {
  it('formats seconds, minutes and hours', () => {
    expect(formatDuration(45)).toBe('45 s')
    expect(formatDuration(60)).toBe('1 min')
    expect(formatDuration(90)).toBe('1 min 30 s')
    expect(formatDuration(3900)).toBe('1 h 5 min')
    expect(formatDuration(-5)).toBe('0 s')
  })
})

describe('dates', () => {
  it('parses ISO strings and rejects garbage', () => {
    expect(parseDate('2026-10-24T09:15:30Z')?.toISOString()).toBe('2026-10-24T09:15:30.000Z')
    expect(parseDate('not a date')).toBeNull()
    expect(parseDate(null)).toBeNull()
  })
  it('formats a local date time', () => {
    expect(formatDateTime(new Date('2026-10-24T09:15:30Z'), 'en')).toContain('2026')
  })
})
