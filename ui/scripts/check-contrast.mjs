// Fails (exit 1) when a foreground/background token pair of src/styles/tokens.css is below
// its WCAG minimum in the light or the dark theme. Runs as part of `npm run lint`.
//
//   node scripts/check-contrast.mjs            summary plus every failure
//   node scripts/check-contrast.mjs --verbose  every pair
import { readFileSync } from 'node:fs'
import { evaluate } from './contrast-lib.mjs'

const verbose = process.argv.includes('--verbose')
const css = readFileSync(new URL('../src/styles/tokens.css', import.meta.url), 'utf8')
const { results, errors } = evaluate(css)

const failures = results.filter((r) => !r.pass)
for (const r of results) {
  if (!verbose && r.pass) continue
  const status = r.pass ? 'ok  ' : 'FAIL'
  console.log(
    `${status} ${r.theme.padEnd(5)} ${r.ratio.toFixed(2).padStart(5)}:1 (min ${r.min}) ` +
      `${r.fg} ${r.fgValue} on ${r.bg} ${r.bgValue}  [${r.why}]`,
  )
}
for (const e of errors) console.log(`ERROR ${e}`)

const lightMin = Math.min(...results.filter((r) => r.theme === 'light').map((r) => r.ratio))
const darkMin = Math.min(...results.filter((r) => r.theme === 'dark').map((r) => r.ratio))
console.log(
  `contrast: ${results.length} pair checks in 2 themes, ${failures.length} below minimum, ` +
    `${errors.length} structural errors (lowest ratio light ${lightMin.toFixed(2)}, dark ${darkMin.toFixed(2)})`,
)
process.exit(failures.length > 0 || errors.length > 0 ? 1 : 0)
