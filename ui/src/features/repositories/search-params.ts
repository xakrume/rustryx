/**
 * Hand written search validation for the content routes (no zod in route files: the router
 * needs it on the critical path, docs/ui-architecture.md section 16). The router parses
 * `?q=123` into the number 123, so every text value is read back as a string.
 */
export function text(value: unknown): string | undefined {
  if (typeof value === 'string') return value === '' ? undefined : value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return undefined
}
