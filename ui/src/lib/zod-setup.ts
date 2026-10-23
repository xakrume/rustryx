import { z } from 'zod'

/**
 * Turn off zod's JIT compiler. Zod 4 probes for `new Function('')` to decide whether it may
 * generate specialised parsers. Under the production Content-Security-Policy (no
 * 'unsafe-eval', scripts/csp.mjs) the probe fails, which is harmless for the result but is
 * reported as a CSP violation on every page that loads a schema, and the policy must stay
 * free of violations to be trusted. The interpreted parser is plenty fast for form sized data.
 *
 * Import this module for its side effect BEFORE the first schema is created:
 * `import '@/lib/zod-setup'` at the top of every module that builds a zod schema.
 */
z.config({ jitless: true })

export { z }
