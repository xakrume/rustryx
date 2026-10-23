import { http, HttpResponse, type HttpHandler } from 'msw'
import { requireSession } from './auth-guard'
import { problem } from './problem'
import operationsFile from './generated/operations.json'

interface GeneratedOperation {
  operationId: string
  method: 'get' | 'post' | 'put' | 'patch' | 'delete'
  path: string
  status: number
  anonymous: boolean
  example?: unknown
}

const operations = (operationsFile as unknown as { operations: GeneratedOperation[] }).operations

/** A short stable tag for an ETag header. Not a security hash: the mock only needs equality. */
function tag(value: unknown): string {
  const text = JSON.stringify(value) ?? ''
  let h = 5381
  for (let i = 0; i < text.length; i += 1) h = (h * 33) ^ text.charCodeAt(i)
  return `"mock-${(h >>> 0).toString(16)}"`
}

/**
 * One handler per operation of the OpenAPI contract that has a response example, answering
 * with that example. Generated from docs/openapi/rustryx-api-v1.yaml (npm run gen:api), so the
 * whole UI runs offline and a new endpoint is mockable the day it enters the contract.
 * They are listed AFTER the hand written handlers (first match wins), so a stateful flow
 * overrides its generated shadow.
 *
 * Contract rules the mock keeps: protected operations need a session (401), cookie sessions
 * need the CSRF header on unsafe methods (403), single objects carry an ETag, PUT and PATCH
 * need If-Match (428 when missing, 412 when it is the literal "stale", which tests use to
 * trigger the conflict dialog).
 */
export function generatedHandlers(): HttpHandler[] {
  return operations.map((op) => {
    const path = op.path.replace(/\{(\w+)\}/g, ':$1')
    return http[op.method](path, ({ request }) => {
      if (!op.anonymous) {
        const guard = requireSession(request)
        if (guard instanceof Response) return guard
      }
      const headers: Record<string, string> = {}
      const single = /\}$/.test(op.path)
      if (op.method === 'put' || op.method === 'patch') {
        const ifMatch = request.headers.get('if-match')
        if (ifMatch === null) {
          return problem(428, 'precondition_required', 'The request needs an If-Match header')
        }
        if (ifMatch === '"stale"') {
          return problem(412, 'precondition_failed', 'The object was changed by someone else')
        }
      }
      if (op.method !== 'delete' && single) headers.ETag = tag(op.example)
      if (op.status === 204 || op.example === undefined) {
        return new HttpResponse(null, { status: op.status, headers })
      }
      return HttpResponse.json(op.example, { status: op.status, headers })
    })
  })
}
