// @vitest-environment node
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { contrastRatio, evaluate, luminance, parseHex, parseThemes } from './contrast-lib.mjs'

const tokensCss = readFileSync(new URL('../src/styles/tokens.css', import.meta.url), 'utf8')

describe('contrast math', () => {
  it('matches the WCAG reference values', () => {
    expect(contrastRatio('#000000', '#ffffff')).toBeCloseTo(21, 5)
    expect(contrastRatio('#ffffff', '#ffffff')).toBeCloseTo(1, 5)
    // #767676 on white is the well known 4.54:1 boundary of AA text.
    expect(contrastRatio('#767676', '#ffffff')).toBeCloseTo(4.54, 2)
    expect(luminance('#fff')).toBeCloseTo(1, 5)
  })

  it('parses short hex and rejects values with alpha', () => {
    expect(parseHex('#0f0')).toEqual([0, 255, 0])
    expect(() => parseHex('#00000080')).toThrow()
    expect(() => parseHex('rgb(0 0 0)')).toThrow()
  })
})

describe('design tokens', () => {
  it('defines exactly the same color tokens in the light and the dark theme', () => {
    const { light, dark } = parseThemes(tokensCss)
    expect([...light.keys()].sort()).toEqual([...dark.keys()].sort())
    expect(light.size).toBeGreaterThan(30)
  })

  it('meets WCAG AA for every checked pair in both themes', () => {
    const { results, errors } = evaluate(tokensCss)
    expect(errors).toEqual([])
    const failing = results.filter((r) => !r.pass).map((r) => `${r.theme} ${r.fg} on ${r.bg}`)
    expect(failing).toEqual([])
    expect(results.some((r) => r.theme === 'light')).toBe(true)
    expect(results.some((r) => r.theme === 'dark')).toBe(true)
  })

  it('must_not_pass_a_low_contrast_pair', () => {
    // A light blue such as #5eb0ff with white text is about 2.2:1: the checker has to reject it.
    const broken = tokensCss.replace('--color-accent: #0071e3;', '--color-accent: #5eb0ff;')
    const { results } = evaluate(broken)
    const bad = results.filter((r) => !r.pass && r.theme === 'light' && r.fg === 'on-accent')
    expect(bad).toHaveLength(1)
  })

  it('must_not_accept_a_token_that_skips_the_proof', () => {
    const extra = tokensCss
      .replace('@theme {', '@theme {\n  --color-mystery: #123456;')
      .replace(
        '--color-background: #000000;',
        '--color-background: #000000;\n  --color-mystery: #654321;',
      )
    const { errors } = evaluate(extra)
    expect(errors.some((e) => e.includes('--color-mystery'))).toBe(true)
  })

  it('must_not_accept_a_theme_that_misses_a_token', () => {
    const broken = tokensCss.replace(/ {2}--color-chart-6: #ff8a50;\n/, '')
    const { errors } = evaluate(broken)
    expect(errors.some((e) => e.includes('--color-chart-6'))).toBe(true)
  })
})
