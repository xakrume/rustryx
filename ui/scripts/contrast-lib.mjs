// WCAG 2.2 contrast checking for the design tokens (src/styles/tokens.css).
//
// Pure functions, no I/O, so the rules are unit tested (contrast-lib.test.mjs) and the CLI
// (check-contrast.mjs) stays a thin wrapper. The CLI runs in `npm run lint`, which makes a
// token edit that drops a pair below the minimum fail the build.

export const TEXT_MIN = 4.5 // WCAG 1.4.3, normal text
export const UI_MIN = 3 // WCAG 1.4.11, control boundaries, focus indicator, graphics

const HEX = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i

/** Parse #rgb or #rrggbb to [r, g, b] in 0..255. Throws on anything else (alpha included). */
export function parseHex(value) {
  const v = value.trim()
  if (!HEX.test(v)) throw new Error(`not a solid hex color: ${value}`)
  const h = v.slice(1)
  const full = h.length === 3 ? [...h].map((c) => c + c).join('') : h
  return [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16))
}

function channel(c8) {
  const c = c8 / 255
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
}

/** Relative luminance as defined by WCAG. */
export function luminance(hex) {
  const [r, g, b] = parseHex(hex)
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/** Contrast ratio between two solid colors, 1..21. */
export function contrastRatio(a, b) {
  const la = luminance(a)
  const lb = luminance(b)
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la]
  return (hi + 0.05) / (lo + 0.05)
}

/** Text on a surface needs 4.5, a control boundary or graphic needs 3. */
const text = (fg, bg, why) => ({ fg, bg, min: TEXT_MIN, kind: 'text', why })
const ui = (fg, bg, why) => ({ fg, bg, min: UI_MIN, kind: 'ui', why })

const SURFACES = ['background', 'card', 'muted']
const STATUS = ['success', 'warning', 'destructive', 'info']

function buildPairs() {
  const pairs = [
    text('foreground', 'background', 'body text'),
    text('foreground', 'card', 'text on cards and panels'),
    text('foreground', 'muted', 'text on muted surfaces (table header, hover rows)'),
    text('card-foreground', 'card', 'card text'),
    text('popover-foreground', 'popover', 'menu and dialog text'),
    text('muted-foreground', 'background', 'secondary text on the page'),
    text('muted-foreground', 'card', 'secondary text on cards'),
    text('muted-foreground', 'muted', 'secondary text on muted surfaces'),
    text('muted-foreground', 'popover', 'secondary text in menus and dialogs'),
    text('on-primary', 'primary', 'text on primary surfaces (active nav item)'),
    text('on-secondary', 'secondary', 'text on secondary buttons'),
    text('on-accent', 'accent', 'text on primary action buttons'),
    text('on-destructive', 'destructive', 'text on destructive buttons'),
    ...SURFACES.map((s) => text('link', s, `links on ${s}`)),
    // Boundaries and indicators.
    ...['background', 'card', 'popover'].map((s) => ui('input', s, `control boundary on ${s}`)),
    ...['background', 'card', 'popover', 'muted'].map((s) => ui('ring', s, `focus ring on ${s}`)),
    ui('accent', 'background', 'accent indicators on the page'),
    ui('accent', 'card', 'accent indicators on cards'),
  ]
  for (const s of STATUS) {
    const solid = s
    const subtle = `${s}-subtle`
    const label = `${s}-text`
    for (const surface of ['background', 'card']) {
      pairs.push(text(label, surface, `${s} text on ${surface}`))
      pairs.push(ui(solid, surface, `${s} icon or dot on ${surface}`))
    }
    pairs.push(text(label, subtle, `${s} badge text on its tint`))
    pairs.push(ui(solid, subtle, `${s} icon on its tint`))
  }
  for (let i = 1; i <= 6; i += 1) {
    // Text level (4.5): the series color also colors the direct label next to the line end.
    pairs.push(text(`chart-${i}`, 'card', `chart series ${i} line and label on cards`))
    pairs.push(text(`chart-${i}`, 'background', `chart series ${i} line and label on the page`))
  }
  return pairs
}

export const PAIRS = buildPairs()

/**
 * Tokens that are deliberately not part of a foreground/background pair, with the reason.
 * A token that is neither paired nor listed here fails the check, so a new token cannot
 * skip the proof by accident.
 */
export const EXEMPT = {
  border: 'decorative separator (WCAG 1.4.11 exempts it); control edges use input',
  overlay: 'translucent scrim behind dialogs, carries no information',
  'chart-grid': 'decorative grid lines, the data table gives the exact values',
  primary: 'surface, proven as the background of on-primary',
  secondary: 'surface, proven as the background of on-secondary',
  accent: 'proven both as indicator and as the background of on-accent',
  destructive: 'proven both as graphic and as the background of on-destructive',
}

/** Find the text of the first block whose header matches `header` (brace matched). */
function blockBody(css, header) {
  const start = css.search(header)
  if (start < 0) throw new Error(`block not found: ${header}`)
  const open = css.indexOf('{', start)
  let depth = 0
  for (let i = open; i < css.length; i += 1) {
    if (css[i] === '{') depth += 1
    else if (css[i] === '}') {
      depth -= 1
      if (depth === 0) return css.slice(open + 1, i)
    }
  }
  throw new Error(`unterminated block: ${header}`)
}

/** Map of token name (without --color-) to raw value, for one block. */
function colorTokens(body) {
  const tokens = new Map()
  for (const m of body.matchAll(/--color-([a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    if (m[1] === '*') continue
    tokens.set(m[1], m[2].trim())
  }
  return tokens
}

/** Light is the `@theme` block, dark is `:root[data-theme='dark']`. */
export function parseThemes(css) {
  return {
    light: colorTokens(blockBody(css, /@theme\s*\{/)),
    dark: colorTokens(blockBody(css, /:root\[data-theme=['"]dark['"]\]\s*\{/)),
  }
}

/**
 * Evaluate every pair in both themes.
 * Returns { results, errors }: results has one row per theme and pair, errors holds
 * structural problems (missing or extra tokens, non-hex values in a pair, unpaired tokens).
 */
export function evaluate(css) {
  const themes = parseThemes(css)
  const results = []
  const errors = []

  const lightNames = [...themes.light.keys()].sort()
  const darkNames = [...themes.dark.keys()].sort()
  for (const n of lightNames) if (!themes.dark.has(n)) errors.push(`dark theme misses --color-${n}`)
  for (const n of darkNames)
    if (!themes.light.has(n)) errors.push(`light theme misses --color-${n}`)

  const paired = new Set()
  for (const [theme, tokens] of Object.entries(themes)) {
    for (const pair of PAIRS) {
      paired.add(pair.fg)
      paired.add(pair.bg)
      const fg = tokens.get(pair.fg)
      const bg = tokens.get(pair.bg)
      if (fg === undefined || bg === undefined) {
        errors.push(`${theme}: pair ${pair.fg} on ${pair.bg} references an undefined token`)
        continue
      }
      try {
        const ratio = contrastRatio(fg, bg)
        results.push({ theme, ...pair, fgValue: fg, bgValue: bg, ratio, pass: ratio >= pair.min })
      } catch (err) {
        errors.push(`${theme}: ${pair.fg} on ${pair.bg}: ${err.message}`)
      }
    }
  }
  for (const name of lightNames) {
    if (!paired.has(name) && !(name in EXEMPT)) {
      errors.push(`--color-${name} is not in any checked pair and not exempt (see EXEMPT)`)
    }
  }
  return { results, errors }
}
