// Fails (exit 1) when an installed dependency, direct or transitive, has a license outside
// scripts/license-policy.mjs: GPL, LGPL, AGPL, unknown or unlicensed.
//   npm run license:check
import { fileURLToPath } from 'node:url'
import * as checker from 'license-checker-rseidelsohn'
import { isAllowed, normalise } from './license-policy.mjs'

const uiRoot = fileURLToPath(new URL('..', import.meta.url))

// Packages whose metadata carries no usable license field but whose license was read by a
// human. Each entry needs the evidence; an empty list is the goal.
const REVIEWED = {}

const packages = await new Promise((resolve, reject) => {
  checker.init({ start: uiRoot, excludePrivatePackages: true }, (err, found) =>
    err ? reject(err) : resolve(found),
  )
})

const counts = new Map()
const violations = []
for (const [id, info] of Object.entries(packages)) {
  const license = REVIEWED[id]?.license ?? normalise(info.licenses)
  counts.set(license, (counts.get(license) ?? 0) + 1)
  if (!isAllowed(license)) violations.push(`${id}: ${license || '(none)'}`)
}

for (const [license, n] of [...counts].sort((a, b) => b[1] - a[1])) {
  console.log(`${String(n).padStart(4)}  ${license}`)
}
console.log(`licenses: ${Object.keys(packages).length} packages, ${violations.length} violations`)
if (violations.length > 0) {
  console.log(violations.join('\n'))
  process.exit(1)
}
