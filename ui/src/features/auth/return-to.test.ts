import { describe, expect, it } from 'vitest'
import { sanitizeReturnTo } from './return-to'

describe('sanitizeReturnTo', () => {
  it('keeps plain absolute paths with query and hash', () => {
    expect(sanitizeReturnTo('/repositories')).toBe('/repositories')
    expect(sanitizeReturnTo('/search?q=maven&format=npm#top')).toBe(
      '/search?q=maven&format=npm#top',
    )
    expect(sanitizeReturnTo('/')).toBe('/')
  })

  it('must_not_accept_anything_that_could_leave_the_site', () => {
    for (const bad of [
      '//evil.example',
      '//evil.example/path',
      '/\\evil.example',
      'https://evil.example',
      'javascript:alert(1)',
      'evil.example/path',
      '',
      '/path\nwith newline',
      `/${'a'.repeat(600)}`,
    ]) {
      expect(sanitizeReturnTo(bad)).toBe('/')
    }
  })

  it('never returns to the login page (a loop)', () => {
    expect(sanitizeReturnTo('/login')).toBe('/')
    expect(sanitizeReturnTo('/login?return_to=/x')).toBe('/')
  })

  it('uses the given fallback and tolerates missing values', () => {
    expect(sanitizeReturnTo(undefined, '/home')).toBe('/home')
    expect(sanitizeReturnTo(null)).toBe('/')
  })
})
