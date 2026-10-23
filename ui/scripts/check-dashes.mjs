// CONTRIBUTING rule 9: no em dash and no en dash anywhere, code and docs included.
// Scans the UI project and the UI architecture document; exits 1 and lists every hit.
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { extname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const uiRoot = fileURLToPath(new URL('..', import.meta.url))
const extraFiles = [fileURLToPath(new URL('../../docs/ui-architecture.md', import.meta.url))]
const skipDirs = new Set([
  'node_modules',
  'dist',
  'dist-mock',
  'playwright-report',
  'test-results',
  'coverage',
  '.vite',
])
const textExt = new Set([
  '.ts',
  '.tsx',
  '.js',
  '.mjs',
  '.json',
  '.css',
  '.html',
  '.md',
  '.yaml',
  '.yml',
  '.svg',
  '.txt',
  '.example',
])
// U+2013 (en dash) and U+2014 (em dash), built from code points so this file passes its own check.
const DASH = new RegExp(`[${String.fromCharCode(0x2013)}${String.fromCharCode(0x2014)}]`)

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    if (skipDirs.has(name)) continue
    const full = join(dir, name)
    if (statSync(full).isDirectory()) yield* walk(full)
    else if (textExt.has(extname(name)) || name === '.env.example') yield full
  }
}

function exists(path) {
  try {
    return statSync(path).isFile()
  } catch {
    return false
  }
}

const hits = []
for (const file of [...walk(uiRoot), ...extraFiles.filter(exists)]) {
  const lines = readFileSync(file, 'utf8').split('\n')
  lines.forEach((line, i) => {
    if (DASH.test(line))
      hits.push(`${relative(uiRoot, file)}:${i + 1}: ${line.trim().slice(0, 100)}`)
  })
}

if (hits.length > 0) {
  console.log(hits.join('\n'))
  console.log(`dashes: ${hits.length} line(s) contain an em or en dash (CONTRIBUTING rule 9)`)
  process.exit(1)
}
console.log('dashes: none found')
