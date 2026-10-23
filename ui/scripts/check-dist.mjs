// Verifies a build output directory against what the Rust server relies on, so a change that
// would break the strict Content-Security-Policy or leak test code into production fails the
// build instead of surfacing as a blank page in a customer's browser.
//
//   node scripts/check-dist.mjs dist production
//   node scripts/check-dist.mjs dist-mock mock
//
// Checks: no inline script, style or event handler in index.html; no external host and no
// data: URI in CSS; hashed asset names; no MSW, fixture or demo code in production; and the
// size of the initial JavaScript (entry plus its static imports), reported and budgeted.
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { gzipSync } from 'node:zlib'

const [dirArg, mode = 'production'] = process.argv.slice(2)
if (!dirArg || !['production', 'mock'].includes(mode)) {
  console.error('usage: check-dist.mjs <dir> <production|mock>')
  process.exit(2)
}
const dir = join(process.cwd(), dirArg)

/** Gzip size budget of the initial JavaScript in production, in KiB. Set from a measurement
 *  (docs/ui-architecture.md records the number) with a little headroom; raising it needs a
 *  reason in the commit message. */
const INITIAL_JS_GZIP_BUDGET_KIB = 175

const failures = []
const fail = (message) => failures.push(message)

function* walk(directory) {
  for (const name of readdirSync(directory)) {
    const full = join(directory, name)
    if (statSync(full).isDirectory()) yield* walk(full)
    else yield full
  }
}

if (!existsSync(join(dir, 'index.html'))) {
  console.error(`check-dist: ${dirArg}/index.html does not exist, build first`)
  process.exit(2)
}

const html = readFileSync(join(dir, 'index.html'), 'utf8')
const files = [...walk(dir)]

// 1. index.html: nothing the CSP would block.
if (/<script\b(?![^>]*\bsrc=)[^>]*>/i.test(html)) fail('index.html has an inline <script>')
if (/<style\b/i.test(html)) fail('index.html has an inline <style>')
if (/\sstyle\s*=/i.test(html)) fail('index.html has a style= attribute')
if (/\son[a-z]+\s*=/i.test(html)) fail('index.html has an inline event handler attribute')
if (/javascript:/i.test(html)) fail('index.html has a javascript: URL')
for (const match of html.matchAll(/(?:src|href)\s*=\s*"([^"]+)"/gi)) {
  const target = match[1]
  if (/^https?:\/\//i.test(target) || target.startsWith('//')) {
    fail(`index.html loads an external resource: ${target}`)
  } else if (target.startsWith('/') && !existsSync(join(dir, target.split('?')[0]))) {
    fail(`index.html references a missing file: ${target}`)
  }
}

// 2. CSS: self hosted fonts, no data: URI (build.assetsInlineLimit is 0), no foreign host.
for (const file of files.filter((f) => f.endsWith('.css'))) {
  const css = readFileSync(file, 'utf8')
  const name = relative(dir, file)
  if (/url\(\s*["']?data:/i.test(css)) fail(`${name} contains a data: URI`)
  if (/(?:@import|url\()\s*["']?\s*(?:https?:)?\/\//i.test(css))
    fail(`${name} loads an external resource`)
}

// 3. No third party font CDN anywhere (CSS and JS).
for (const file of files.filter((f) => /\.(css|js|html)$/.test(f))) {
  const text = readFileSync(file, 'utf8')
  if (/fonts\.(googleapis|gstatic)\.com/.test(text))
    fail(`${relative(dir, file)} references a font CDN`)
}

// 4. Hashed assets: the server may cache them forever, which is only safe with a content hash.
for (const file of files.filter((f) => relative(dir, f).startsWith('assets/'))) {
  if (!/-[A-Za-z0-9_-]{8,}\.[a-z0-9]+$/.test(file))
    fail(`${relative(dir, file)} has no content hash`)
}
for (const required of ['theme-init.js', 'favicon.svg']) {
  if (!existsSync(join(dir, required))) fail(`${required} is missing from the build output`)
}

// 5. Mode specific: production has no mock and no demo, mock has the worker.
const jsFiles = files.filter((f) => f.endsWith('.js') && relative(dir, f).startsWith('assets/'))
if (mode === 'production') {
  if (existsSync(join(dir, 'mockServiceWorker.js')))
    fail('mockServiceWorker.js is in the production build')
  for (const file of jsFiles) {
    const text = readFileSync(file, 'utf8')
    if (/mockServiceWorker|@mswjs|ryx-mock-session|mock-password/.test(text)) {
      fail(`${relative(dir, file)} contains mock code`)
    }
    if (text.includes('Every shared component in one place')) {
      fail(`${relative(dir, file)} contains the component gallery`)
    }
  }
} else if (!existsSync(join(dir, 'mockServiceWorker.js'))) {
  fail('mockServiceWorker.js is missing from the mock build')
}

// 6. Initial JavaScript: the entry script and everything it imports statically.
const initial = new Set()
for (const m of html.matchAll(
  /<script[^>]*\ssrc="([^"]+)"[^>]*type="module"|<script[^>]*type="module"[^>]*\ssrc="([^"]+)"/gi,
)) {
  initial.add(m[1] ?? m[2])
}
for (const m of html.matchAll(/<link[^>]*rel="modulepreload"[^>]*href="([^"]+)"/gi))
  initial.add(m[1])
const parts = []
let rawBytes = 0
let gzipBytes = 0
for (const href of initial) {
  const file = join(dir, href)
  if (!existsSync(file)) continue
  const buffer = readFileSync(file)
  rawBytes += buffer.length
  const gz = gzipSync(buffer).length
  gzipBytes += gz
  parts.push(
    `${href} ${(buffer.length / 1024).toFixed(1)} KiB raw, ${(gz / 1024).toFixed(1)} KiB gzip`,
  )
}
const cssFiles = [...html.matchAll(/<link[^>]*rel="stylesheet"[^>]*href="([^"]+)"/gi)].map(
  (m) => m[1],
)
let cssGzip = 0
for (const href of cssFiles) cssGzip += gzipSync(readFileSync(join(dir, href))).length
const kib = (n) => (n / 1024).toFixed(1)
console.log(
  `check-dist ${dirArg} (${mode}): initial JS ${kib(rawBytes)} KiB raw, ${kib(gzipBytes)} KiB gzip ` +
    `(${initial.size} files); initial CSS ${kib(cssGzip)} KiB gzip; ${jsFiles.length} JS chunks`,
)
for (const part of parts) console.log(`  ${part}`)
if (mode === 'production' && gzipBytes / 1024 > INITIAL_JS_GZIP_BUDGET_KIB) {
  fail(
    `initial JS is ${kib(gzipBytes)} KiB gzip, over the budget of ${INITIAL_JS_GZIP_BUDGET_KIB} KiB`,
  )
}

if (failures.length > 0) {
  for (const message of failures) console.error(`check-dist FAIL: ${message}`)
  process.exit(1)
}
console.log('check-dist: ok')
