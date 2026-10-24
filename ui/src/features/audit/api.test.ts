import { describe, expect, it } from 'vitest'
import { auditQuery, resolveBounds } from './api'
import { isoToLocalInput, localInputToIso } from './time-input'

const NOW = Date.parse('2026-10-24T12:00:00Z')

describe('resolveBounds', () => {
  it('anchors a relative range at the given instant, once', () => {
    expect(resolveBounds({ range: 'hour' }, NOW)).toEqual({ from: '2026-10-24T11:00:00Z' })
    expect(resolveBounds({ range: 'day' }, NOW)).toEqual({ from: '2026-10-23T12:00:00Z' })
    expect(resolveBounds({ range: 'week' }, NOW)).toEqual({ from: '2026-10-17T12:00:00Z' })
    expect(resolveBounds({ range: 'month' }, NOW)).toEqual({ from: '2026-09-24T12:00:00Z' })
  })

  it('takes a custom range as given and ignores the dates of a preset', () => {
    expect(resolveBounds({ range: 'custom', from: 'a', to: 'b' }, NOW)).toEqual({
      from: 'a',
      to: 'b',
    })
    expect(resolveBounds({ range: 'week', from: 'a', to: 'b' }, NOW).to).toBeUndefined()
    expect(resolveBounds({}, NOW)).toEqual({})
    expect(resolveBounds({ range: 'bogus' }, NOW)).toEqual({})
  })
})

describe('auditQuery', () => {
  it('turns the address filters into API parameters and drops what the API would refuse', () => {
    const query = auditQuery(
      {
        kind: 'rbac_change,nonsense,token_change',
        actor: 'al',
        outcome: 'failure',
        request_id: 'r',
      },
      { from: 'f', to: 't' },
    )
    expect(query).toMatchObject({
      kind: ['rbac_change', 'token_change'],
      actor: 'al',
      outcome: 'failure',
      request_id: 'r',
      from: 'f',
      to: 't',
    })
    expect(auditQuery({ outcome: 'weird', kind: '' }, {})).toMatchObject({
      kind: undefined,
      outcome: undefined,
    })
  })
})

describe('datetime-local conversion', () => {
  it('round trips a local time through an instant', () => {
    const iso = localInputToIso('2026-10-24T09:15')
    expect(iso).toMatch(/^2026-10-24T\d\d:\d\d:00Z$/)
    expect(isoToLocalInput(iso ?? undefined)).toBe('2026-10-24T09:15')
  })

  it('treats empty and invalid values as no value', () => {
    expect(localInputToIso('')).toBeNull()
    expect(localInputToIso('not a time')).toBeNull()
    expect(isoToLocalInput(undefined)).toBe('')
    expect(isoToLocalInput('nope')).toBe('')
  })
})
