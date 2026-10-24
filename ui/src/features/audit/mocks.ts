import { delay, http, HttpResponse, type HttpHandler } from 'msw'
import {
  handle,
  MockProblem,
  pageResponse,
  slice,
  startsWithText,
} from '@/features/roles/mock-http'
import type { AuditRecord } from '@/features/roles/mock-seed'
import { identity } from '@/features/roles/mock-store'
import { LATENCY_MS } from '@/mocks/store'

const MAX_EXPORT_DAYS = 366

function filtered(url: URL): AuditRecord[] {
  const p = url.searchParams
  const kinds = p.get('kind')?.split(',').filter(Boolean)
  const actor = p.get('actor')
  const action = p.get('action')
  const target = p.get('target')
  const repository = p.get('repository')
  const project = p.get('project')
  const requestId = p.get('request_id')
  const outcome = p.get('outcome')
  const from = p.get('from')
  const to = p.get('to')
  return identity().audit.filter(
    (e) =>
      (kinds === undefined || kinds.includes(e.kind)) &&
      (actor === null || startsWithText(e.actor_name, actor)) &&
      (action === null ||
        (action.endsWith('.') ? e.action.startsWith(action) : e.action === action)) &&
      (target === null || e.target === target) &&
      (repository === null ||
        e.target === repository ||
        (e.target ?? '').startsWith(`${repository}/`)) &&
      (project === null || e.detail.project === project) &&
      (requestId === null || e.request_id === requestId) &&
      (outcome === null || e.outcome === outcome) &&
      (from === null || e.at >= from) &&
      (to === null || e.at < to),
  )
}

/** The cut of an export: set `audit-export:cut` in `ryx-mock-chaos` and the next stream breaks halfway. */
function takeCut(): boolean {
  try {
    const raw = window.sessionStorage.getItem('ryx-mock-chaos') ?? ''
    if (
      !raw
        .split(',')
        .map((s) => s.trim())
        .includes('audit-export:cut')
    )
      return false
    window.sessionStorage.setItem(
      'ryx-mock-chaos',
      raw
        .split(',')
        .filter((s) => s.trim() !== 'audit-export:cut')
        .join(','),
    )
    return true
  } catch {
    return false
  }
}

const encoder = new TextEncoder()

/** Stateful mock of the audit log: filters, keyset pages, and a streamed NDJSON export. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/audit-events/export', ({ request }) =>
    handle(request, { feature: 'audit' }, () => {
      const url = new URL(request.url)
      const from = url.searchParams.get('from')
      const to = url.searchParams.get('to')
      if (
        from !== null &&
        to !== null &&
        Date.parse(to) - Date.parse(from) > MAX_EXPORT_DAYS * 86_400_000
      ) {
        throw new MockProblem(400, 'range_too_large', 'The range is too large', {
          details: { max_days: MAX_EXPORT_DAYS },
        })
      }
      const after = url.searchParams.get('after')
      // Oldest first, each line carries the cursor just after it (api-conventions, 15).
      const events = filtered(url)
        .slice()
        .reverse()
        .map((e, i, all) => ({
          e,
          cursor: all.length === 0 ? '' : btoa(e.id).replace(/=+$/, ''),
          i,
        }))
      const start = after === null ? 0 : events.findIndex((x) => x.cursor === after) + 1
      const rows = events.slice(start)
      const cut = takeCut()
      const limit = cut ? Math.max(1, Math.floor(rows.length / 2)) : rows.length
      let index = 0
      const stream = new ReadableStream<Uint8Array>({
        async pull(controller) {
          if (LATENCY_MS > 0) await delay(15)
          const chunk = rows.slice(index, index + 25)
          index += chunk.length
          if (chunk.length === 0 || index > limit) {
            if (cut && index > limit) {
              // A broken connection: the last line is cut in the middle and no newline follows it.
              const partial = JSON.stringify({ ...rows[limit]?.e, cursor: rows[limit]?.cursor })
              controller.enqueue(encoder.encode(partial.slice(0, partial.length / 2)))
              controller.error(new TypeError('network error'))
              return
            }
            controller.close()
            return
          }
          controller.enqueue(
            encoder.encode(
              chunk.map((x) => `${JSON.stringify({ ...x.e, cursor: x.cursor })}\n`).join(''),
            ),
          )
        },
      })
      return new HttpResponse(stream, {
        headers: {
          'Content-Type': 'application/x-ndjson',
          'Content-Disposition': 'attachment; filename=audit-events.ndjson',
          'Cache-Control': 'no-store',
        },
      })
    }),
  ),

  http.get('/api/v1/audit-events/:eventId', ({ request, params }) =>
    handle(request, { feature: 'audit' }, () => {
      const event = identity().audit.find((e) => e.id === params.eventId)
      if (!event) {
        throw new MockProblem(404, 'audit_event_not_found', 'The audit event does not exist')
      }
      return HttpResponse.json(event)
    }),
  ),

  http.get('/api/v1/audit-events', ({ request }) =>
    handle(request, { feature: 'audit' }, () => {
      const url = new URL(request.url)
      const sort = url.searchParams.get('sort') ?? '-at'
      if (sort !== '-at' && sort !== 'at') {
        throw new MockProblem(400, 'unsupported_sort', 'The endpoint cannot sort by this field')
      }
      const events = filtered(url).sort((a, b) => a.at.localeCompare(b.at))
      if (sort === '-at') events.reverse()
      return pageResponse(slice(events, url, { defaultLimit: 50, maxLimit: 200 }))
    }),
  ),
]
