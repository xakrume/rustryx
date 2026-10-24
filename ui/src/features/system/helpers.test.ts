import { describe, expect, it } from 'vitest'
import { mockId } from './mock-kit'
import { isActiveRun, pollInterval } from './run-status'
import { scalarText } from './scalar-text'
import { attentionChecks, checkById } from './status-model'
import type { StatusCheck } from './api'
import { formatUptime } from './uptime'

const check = (id: string, status: StatusCheck['status']): StatusCheck => ({
  id,
  title: id,
  status,
  severity: 'info',
  message: '',
  fix_hint: null,
  since: null,
  link: null,
})

describe('scalarText', () => {
  it('writes scalars as they are and structures as JSON, never as [object Object]', () => {
    expect(scalarText(null)).toBe('')
    expect(scalarText(undefined)).toBe('')
    expect(scalarText('a')).toBe('a')
    expect(scalarText(3)).toBe('3')
    expect(scalarText(false)).toBe('false')
    expect(scalarText({ a: 1 })).toBe('{"a":1}')
    expect(scalarText([1, 2])).toBe('[1,2]')
  })
})

describe('formatUptime', () => {
  it('uses days and hours for long spans and the shared form for short ones', () => {
    expect(formatUptime(4 * 86_400 + 5_400)).toBe('4 d 1 h')
    expect(formatUptime(2 * 86_400)).toBe('2 d')
    expect(formatUptime(125)).toBe('2 min 5 s')
    expect(formatUptime(-5)).toBe('0 s')
  })
})

describe('run status helpers', () => {
  it('knows which statuses still change', () => {
    expect(isActiveRun('queued')).toBe(true)
    expect(isActiveRun('running')).toBe(true)
    expect(isActiveRun('succeeded')).toBe(false)
    expect(isActiveRun(undefined)).toBe(false)
  })

  it('polls fast in tests; the production schedule is 2 s, 5 s, 15 s', () => {
    expect(pollInterval(0)).toBeLessThan(100)
  })
})

describe('status model', () => {
  it('puts what needs attention first and leaves the dependency probes out', () => {
    const list = attentionChecks([
      check('database', 'error'),
      check('a', 'ok'),
      check('b', 'warning'),
      check('c', 'unknown'),
      check('d', 'error'),
    ])
    expect(list.map((c) => c.id)).toEqual(['d', 'b', 'c', 'a'])
    expect(checkById([check('redis', 'ok')], 'redis')?.status).toBe('ok')
    expect(checkById([], 'redis')).toBeUndefined()
  })
})

describe('mockId', () => {
  it('builds ids in a range of their own', () => {
    expect(mockId(7, 1)).toBe('0192f6c4-5a1e-7000-8007-000000000001')
  })
})
