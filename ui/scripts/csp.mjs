// The Content-Security-Policy the production build is designed for. ONE source:
// vite preview (and so the Playwright smoke test) sends exactly this string,
// scripts/check-dist.mjs verifies the build against it, and docs/ui-architecture.md
// tells the Rust server to send the same value with the static files.
//
// Why each directive is as strict as it is:
// - script-src 'self': no inline script exists. The theme bootstrap is the external
//   file /theme-init.js, not an inline snippet.
// - style-src 'self': no inline <style> and no style="" attribute in markup. React sets
//   style props through the CSSOM (element.style), which CSP does not block. Libraries
//   that inject a <style> element at runtime (react-remove-scroll behind Radix Dialog,
//   sonner, Radix ScrollArea) would be blocked, so the project does not use them.
// - img-src and font-src 'self': fonts are self-hosted and no asset is inlined as a
//   data: URI (vite build.assetsInlineLimit is 0).
// - connect-src 'self': the API is same origin. An API on another origin needs this
//   directive widened by the operator, on purpose.
// - frame-ancestors 'none' and object-src 'none': the UI is never framed or a plugin host.
export const contentSecurityPolicy = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self'",
  "font-src 'self'",
  "connect-src 'self'",
  "worker-src 'self'",
  "manifest-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'self'",
  "frame-ancestors 'none'",
].join('; ')

/** Security headers the server sends next to the policy (documented, not enforced here). */
export const securityHeaders = {
  'Content-Security-Policy': contentSecurityPolicy,
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'X-Frame-Options': 'DENY',
}
