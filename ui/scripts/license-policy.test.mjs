// @vitest-environment node
import { describe, expect, it } from 'vitest'
import { isAllowed } from './license-policy.mjs'

describe('license policy', () => {
  it('accepts the permissive licenses of CONTRIBUTING.md', () => {
    for (const l of ['MIT', 'Apache-2.0', 'BSD-3-Clause', 'ISC', 'MPL-2.0', 'Zlib']) {
      expect(isAllowed(l)).toBe(true)
    }
  })

  it('accepts an OR expression when one alternative is allowed', () => {
    expect(isAllowed('(MIT OR GPL-3.0-only)')).toBe(true)
  })

  it('must_not_accept_gpl_lgpl_or_agpl', () => {
    for (const l of ['GPL-3.0-only', 'GPL-2.0-or-later', 'LGPL-2.1', 'AGPL-3.0-only', 'GPL-3.0+']) {
      expect(isAllowed(l)).toBe(false)
    }
  })

  it('must_not_accept_a_copyleft_part_of_an_AND_expression', () => {
    expect(isAllowed('MIT AND GPL-3.0-only')).toBe(false)
  })

  it('must_not_accept_unknown_or_missing_licenses', () => {
    for (const l of ['', 'UNKNOWN', 'UNLICENSED', 'Custom: https://example.org', undefined]) {
      expect(isAllowed(l)).toBe(false)
    }
  })
})
