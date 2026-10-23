// License policy of the UI dependencies, aligned with CONTRIBUTING.md: licenses must be
// MIT compatible (MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode, MPL-2.0); GPL, LGPL and
// AGPL are never allowed. Pure functions so the policy is unit tested.

/** SPDX identifiers that are fine to ship in an MIT licensed product. */
export const ALLOWED = new Set([
  'MIT',
  'MIT-0',
  'Apache-2.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  '0BSD',
  'ISC',
  'Zlib',
  'BlueOak-1.0.0',
  'CC0-1.0',
  'Unlicense',
  'Python-2.0',
  'Unicode-3.0',
  'Unicode-DFS-2016',
  // MPL-2.0 is file level copyleft and listed as allowed by CONTRIBUTING.md (lightningcss
  // of the Tailwind build, axe-core). Used unmodified, so no obligation reaches our code.
  'MPL-2.0',
  // Data and fonts, not code: caniuse-lite and the SPDX lists of the license checker (both
  // dev only, attribution licenses without copyleft) and the Fira fonts (self hosted, shipped).
  'CC-BY-4.0',
  'CC-BY-3.0',
  'OFL-1.1',
])

/** A license that is never acceptable, whatever else it is combined with by AND. */
const FORBIDDEN = /\b(A?GPL|LGPL)\b/i

/** Normalise the license field of license-checker output to one SPDX-ish string. */
export function normalise(licenses) {
  const value = Array.isArray(licenses) ? licenses.join(' AND ') : String(licenses ?? '')
  return value.replace(/[*()]/g, '').replace(/\s+/g, ' ').trim()
}

/**
 * Is an SPDX expression acceptable? `A OR B` passes when one alternative is allowed (we may
 * pick it), `A AND B` needs both. A forbidden identifier only counts when it is the sole
 * choice, which falls out of the OR rule.
 */
export function isAllowed(expression) {
  const expr = normalise(expression)
  if (expr === '' || /unknown|unlicensed|custom|see license/i.test(expr)) return false
  return expr.split(/ OR /i).some((alternative) =>
    alternative.split(/ AND /i).every((id) => {
      const spdx = id.trim().replace(/\+$/, '')
      return !FORBIDDEN.test(spdx) && ALLOWED.has(spdx)
    }),
  )
}
