// Pulls one success response example per operation out of a parsed OpenAPI document.
// Pure (no I/O) so it is unit tested. The mock server (src/mocks) answers every operation
// that has an example with it, so the UI runs end to end with no backend.

const METHODS = ['get', 'post', 'put', 'patch', 'delete']

/** Resolve a local `#/...` reference. Throws on anything else: the contract has none. */
export function resolveRef(spec, ref) {
  if (!ref.startsWith('#/')) throw new Error(`unsupported $ref: ${ref}`)
  let node = spec
  for (const part of ref.slice(2).split('/')) {
    node = node?.[part.replace(/~1/g, '/').replace(/~0/g, '~')]
  }
  if (node === undefined) throw new Error(`dangling $ref: ${ref}`)
  return node
}

function deref(spec, node, depth = 0) {
  if (node && typeof node === 'object' && typeof node.$ref === 'string') {
    if (depth > 16) throw new Error(`$ref chain too deep at ${node.$ref}`)
    return deref(spec, resolveRef(spec, node.$ref), depth + 1)
  }
  return node
}

/** The example of a schema: its own `example`, or the one of the first allOf member. */
function schemaExample(spec, schema, depth = 0) {
  const s = deref(spec, schema)
  if (!s || typeof s !== 'object' || depth > 8) return undefined
  if (s.example !== undefined) return s.example
  if (Array.isArray(s.allOf)) {
    for (const part of s.allOf) {
      const ex = schemaExample(spec, part, depth + 1)
      if (ex !== undefined) return ex
    }
  }
  return undefined
}

function responseExample(spec, response) {
  const content = deref(spec, response)?.content?.['application/json']
  if (!content) return undefined
  if (content.example !== undefined) return content.example
  if (content.examples && typeof content.examples === 'object') {
    const first = Object.values(content.examples)[0]
    const resolved = deref(spec, first)
    if (resolved?.value !== undefined) return resolved.value
  }
  return schemaExample(spec, content.schema)
}

/** Does the operation work without credentials? Mirrors how the contract spells it. */
function allowsAnonymous(spec, op) {
  const security = op.security ?? spec.security ?? []
  return security.length === 0 || security.some((req) => Object.keys(req).length === 0)
}

/**
 * One entry per operation that has a 2xx response: { operationId, method, path, status,
 * example }. `example` is omitted for bodiless answers (204) and operations whose response
 * schema carries no example. Order follows the document, so regeneration is stable.
 */
export function extractOperations(spec) {
  const out = []
  for (const [path, item] of Object.entries(spec.paths ?? {})) {
    for (const method of METHODS) {
      const op = item[method]
      if (!op?.operationId) continue
      const statuses = Object.keys(op.responses ?? {})
        .filter((s) => /^2\d\d$/.test(s))
        .sort()
      const status = statuses[0]
      if (!status) continue
      const example = responseExample(spec, op.responses[status])
      const entry = {
        operationId: op.operationId,
        method,
        path,
        status: Number(status),
        // The mock answers 401 for a call without a session unless the contract allows anonymous
        // callers: an empty requirement (`{}`) in `security`, or `security: []`.
        anonymous: allowsAnonymous(spec, op),
      }
      if (example !== undefined) entry.example = example
      // A JSON answer without an example is not mockable; a bodiless answer is.
      if (example === undefined && status !== '204') {
        const hasJson = Boolean(deref(spec, op.responses[status])?.content?.['application/json'])
        if (hasJson) continue
      }
      out.push(entry)
    }
  }
  return out
}
