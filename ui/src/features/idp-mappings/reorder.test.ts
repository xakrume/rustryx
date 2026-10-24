import { describe, expect, it } from 'vitest'
import { parseGroups } from './mapping-tester'
import { priorityChanges } from './reorder'

const rows = [
  { id: 'a', priority: 10 },
  { id: 'b', priority: 20 },
  { id: 'c', priority: 20 },
  { id: 'd', priority: 30 },
]

describe('priorityChanges', () => {
  it('swaps two different priorities', () => {
    expect(priorityChanges(rows, 3, -1)).toEqual([
      { id: 'd', priority: 20 },
      { id: 'c', priority: 30 },
    ])
    expect(priorityChanges(rows, 0, 1)).toEqual([
      { id: 'a', priority: 20 },
      { id: 'b', priority: 10 },
    ])
  })

  it('steps past a neighbour with the same priority', () => {
    expect(priorityChanges(rows, 2, -1)).toEqual([{ id: 'c', priority: 19 }])
    expect(priorityChanges(rows, 1, 1)).toEqual([{ id: 'b', priority: 21 }])
  })

  it('changes nothing at the ends', () => {
    expect(priorityChanges(rows, 0, -1)).toEqual([])
    expect(priorityChanges(rows, 3, 1)).toEqual([])
    expect(priorityChanges([], 0, 1)).toEqual([])
  })
})

describe('parseGroups', () => {
  it('keeps every line exactly as typed and drops only empty lines', () => {
    expect(parseGroups('/a\n\n /b \n/C')).toEqual(['/a', ' /b ', '/C'])
    expect(parseGroups('')).toEqual([])
  })
})
