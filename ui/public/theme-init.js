// Sets the theme and density attributes on <html> before the first paint.
//
// Why this is an external classic script: the Content-Security-Policy has no
// 'unsafe-inline', so an inline snippet in index.html is blocked, and a module script
// runs after first paint (a dark mode user would see a light flash). A blocking
// <script src> in <head> is allowed by script-src 'self' and runs before rendering.
//
// The storage keys and values must equal src/lib/constants.ts (STORAGE_KEYS); the unit
// test theme-init.test.ts parses this file and fails when they drift.
//
// Every read is guarded: storage can throw (private mode, blocked site data) and matchMedia
// may be missing. Failure leaves the defaults in index.html (light, compact).
;(function () {
  var root = document.documentElement
  var theme = null
  var density = null
  try {
    theme = window.localStorage.getItem('rustryx.theme')
    density = window.localStorage.getItem('rustryx.density')
  } catch {
    // Storage unavailable: fall through to the system preference and defaults.
  }
  var dark = false
  try {
    dark = window.matchMedia('(prefers-color-scheme: dark)').matches
  } catch {
    // No matchMedia: keep light.
  }
  var resolved = theme === 'light' || theme === 'dark' ? theme : dark ? 'dark' : 'light'
  root.setAttribute('data-theme', resolved)
  root.setAttribute('data-density', density === 'comfortable' ? 'comfortable' : 'compact')
})()
