import { describe, expect, it } from 'vitest'
import { readJson, readStorage, removeStorage, writeJson, writeStorage } from './storage'

describe('storage', () => {
  it('reads and writes strings and JSON', () => {
    expect(writeStorage('k', 'v')).toBe(true)
    expect(readStorage('k')).toBe('v')
    writeJson('j', { a: true })
    const isRecord = (v: unknown): v is { a: boolean } => typeof v === 'object' && v !== null
    expect(readJson('j', isRecord, { a: false })).toEqual({ a: true })
    removeStorage('k')
    expect(readStorage('k')).toBeNull()
  })

  it('must_not_throw_when_storage_throws', () => {
    // Private windows, blocked site data and previews make every access throw.
    const boom = () => {
      throw new DOMException('denied', 'SecurityError')
    }
    const original = window.localStorage
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: { getItem: boom, setItem: boom, removeItem: boom },
    })
    try {
      expect(readStorage('k')).toBeNull()
      expect(writeStorage('k', 'v')).toBe(false)
      expect(() => removeStorage('k')).not.toThrow()
      expect(readJson('k', (v): v is number => typeof v === 'number', 5)).toBe(5)
    } finally {
      Object.defineProperty(window, 'localStorage', { configurable: true, value: original })
    }
  })

  it('returns the fallback for malformed or foreign JSON', () => {
    writeStorage('bad', '{not json')
    writeStorage('foreign', JSON.stringify('a string'))
    const isCount = (v: unknown): v is { n: number } =>
      typeof v === 'object' && v !== null && typeof (v as { n?: unknown }).n === 'number'
    expect(readJson('bad', isCount, { n: 0 })).toEqual({ n: 0 })
    expect(readJson('foreign', isCount, { n: 0 })).toEqual({ n: 0 })
  })
})
