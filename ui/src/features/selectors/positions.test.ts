import { describe, expect, it } from 'vitest'
import { byteLength, byteOffsetToIndex, locate } from './positions'

describe('byte offsets of a compile error', () => {
  it('equal text indexes for ASCII', () => {
    expect(byteOffsetToIndex('abc', 2)).toBe(2)
    expect(byteOffsetToIndex('abc', 99)).toBe(3)
    expect(byteOffsetToIndex('', 0)).toBe(0)
  })

  it('count a multi byte character as several bytes', () => {
    // "é" is 2 bytes, "€" 3 bytes, an emoji 4 bytes and 2 UTF-16 units.
    expect(byteOffsetToIndex('é=1', 2)).toBe(1)
    expect(byteOffsetToIndex('€=1', 3)).toBe(1)
    expect(byteOffsetToIndex('😀=1', 4)).toBe(2)
    // An offset in the middle of a character lands on that character.
    expect(byteOffsetToIndex('€=1', 1)).toBe(0)
    expect(byteLength('é€😀')).toBe(9)
  })

  it('locate gives a one based line and column and the text of the line', () => {
    expect(locate('format == "x"', null)).toBeNull()
    expect(locate('a &&\n  b ||', 9)).toEqual({ index: 9, line: 2, column: 5, lineText: '  b ||' })
    expect(locate('é = 1', 3)).toMatchObject({ line: 1, column: 3 })
  })
})
