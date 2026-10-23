// @vitest-environment node
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { STORAGE_KEYS } from './constants'

const script = readFileSync(new URL('../../public/theme-init.js', import.meta.url), 'utf8')

describe('public/theme-init.js', () => {
  it('reads the same storage keys as the application (one source of truth)', () => {
    expect(script).toContain(`'${STORAGE_KEYS.theme}'`)
    expect(script).toContain(`'${STORAGE_KEYS.density}'`)
  })

  it('must_not_be_inline_in_index.html (the CSP has no unsafe-inline)', () => {
    const html = readFileSync(new URL('../../index.html', import.meta.url), 'utf8')
    expect(html).toContain('<script src="/theme-init.js"></script>')
    expect(html).not.toMatch(/<script(?![^>]*\bsrc=)[^>]*>/)
    expect(html).not.toMatch(/<style/)
  })

  it('guards every storage and matchMedia access', () => {
    expect(script).toMatch(/try\s*{[^}]*localStorage/)
    expect(script).toMatch(/try\s*{[^}]*matchMedia/)
  })
})
