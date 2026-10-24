import { describe, expect, it } from 'vitest'
import { cyclePath } from './cycle'

const roles = [
  { id: 'a', name: 'a', nested_role_ids: ['b'] },
  { id: 'b', name: 'b', nested_role_ids: ['c'] },
  { id: 'c', name: 'c', nested_role_ids: [] },
  { id: 'd', name: 'd', nested_role_ids: [] },
]

describe('cyclePath', () => {
  it('names the chain that would close a cycle', () => {
    // Nesting a into c: a already reaches c through b.
    expect(cyclePath(roles, 'c', 'a')).toEqual(['a', 'b', 'c'])
    expect(cyclePath(roles, 'b', 'a')).toEqual(['a', 'b'])
  })

  it('treats a role holding itself as a cycle', () => {
    expect(cyclePath(roles, 'a', 'a')).toEqual(['a'])
  })

  it('allows a nesting that does not lead back', () => {
    expect(cyclePath(roles, 'a', 'c')).toBeNull()
    expect(cyclePath(roles, 'd', 'a')).toBeNull()
  })

  it('survives a cycle that already exists and unknown ids', () => {
    const looped = [
      { id: 'x', name: 'x', nested_role_ids: ['y'] },
      { id: 'y', name: 'y', nested_role_ids: ['x'] },
      { id: 'z', name: 'z', nested_role_ids: [] },
    ]
    expect(cyclePath(looped, 'z', 'x')).toBeNull()
    expect(cyclePath(looped, 'z', 'missing')).toBeNull()
    expect(cyclePath([{ id: 'p', name: 'p', nested_role_ids: ['gone'] }], 'q', 'p')).toBeNull()
  })
})
