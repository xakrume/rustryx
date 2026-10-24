import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import {
  checkOptionalIfMatch,
  handle,
  jsonWithEtag,
  MockProblem,
  noContent,
  pageResponse,
  readJson,
  requireIfMatch,
  slice,
  startsWithText,
} from '@/features/roles/mock-http'
import {
  audit,
  commit,
  etagOfSelector,
  findSelector,
  freshId,
  identity,
  isoNow,
  selectorView,
  validateSelectorInput,
} from '@/features/roles/mock-store'
import { repositories } from '@/mocks/store'
import { compile, LANGUAGE, verdictOf } from './mock-engine'

type S = components['schemas']

const PREVIEW_PATHS: Record<string, readonly string[]> = {
  maven2: [
    'com/example/lib/1.0/lib-1.0.jar',
    'com/example/lib/1.0/lib-1.0.pom',
    'com/example/app/2.3-SNAPSHOT/app-2.3-SNAPSHOT.jar',
    'org/other/util/4.1/util-4.1.jar',
  ],
  npm: ['left-pad/-/left-pad-1.3.0.tgz', '@example/ui/-/ui-0.4.2.tgz'],
  docker: ['v2/prod/api/manifests/1.4.0', 'v2/dev/api/manifests/latest'],
}

/** Stateful mock of the content selector API, with a small stand-in for the selector engine. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/content-selectors/language', ({ request }) =>
    handle(request, { feature: 'selectors' }, () => HttpResponse.json(LANGUAGE as never)),
  ),

  http.post('/api/v1/content-selectors/validate', ({ request }) =>
    handle(request, { feature: 'selectors' }, async () => {
      const body = await readJson<{ expression?: unknown }>(request)
      const compiled = compile(typeof body.expression === 'string' ? body.expression : '')
      return HttpResponse.json(
        Array.isArray(compiled) ? { valid: false, errors: compiled } : { valid: true, errors: [] },
      )
    }),
  ),

  http.post('/api/v1/content-selectors/test', ({ request }) =>
    handle(request, { feature: 'selectors' }, async () => {
      const body = await readJson<{ expression?: string; samples?: S['SelectorSample'][] }>(request)
      const samples = body.samples ?? []
      if (samples.length < 1 || samples.length > 50) {
        throw new MockProblem(400, 'invalid_input', 'Send between 1 and 50 samples')
      }
      const compiled = compile(body.expression ?? '')
      if (Array.isArray(compiled)) {
        return HttpResponse.json({ valid: false, errors: compiled, results: [] })
      }
      return HttpResponse.json({
        valid: true,
        errors: [],
        results: samples.map((sample, index) => ({ index, ...verdictOf(compiled, sample) })),
      })
    }),
  ),

  http.post('/api/v1/content-selectors/preview', ({ request }) =>
    handle(request, { feature: 'selectors' }, async () => {
      const body = await readJson<{ expression?: string; repository?: string; limit?: number }>(
        request,
      )
      const repository = repositories.find((r) => r.name === body.repository)
      if (!repository) {
        throw new MockProblem(404, 'repository_not_found', 'Repository not found')
      }
      const compiled = compile(body.expression ?? '')
      if (Array.isArray(compiled)) {
        return HttpResponse.json({ valid: false, errors: compiled, items: [], truncated: false })
      }
      const paths = PREVIEW_PATHS[repository.format] ?? ['file-1.bin', 'docs/readme.txt']
      const hits = paths
        .filter(
          (path) =>
            verdictOf(compiled, {
              format: repository.format,
              path: `/${path}`,
              repository: repository.name,
            }).result === 'match',
        )
        .map((path, i) => ({
          asset_id: freshId(),
          path,
          component_id: i % 2 === 0 ? freshId() : null,
        }))
      const limit = Math.min(body.limit ?? 10, 10)
      return HttpResponse.json({
        valid: true,
        errors: [],
        items: hits.slice(0, limit),
        truncated: hits.length > limit,
      })
    }),
  ),

  http.get('/api/v1/content-selectors', ({ request }) =>
    handle(request, { feature: 'selectors' }, () => {
      const url = new URL(request.url)
      const q = url.searchParams.get('q')
      const items = identity()
        .selectors.filter((s) => (q === null ? true : startsWithText(s.name, q)))
        .sort((a, b) => a.name.localeCompare(b.name))
        .map(selectorView)
      return pageResponse(slice(items, url))
    }),
  ),

  http.post('/api/v1/content-selectors', ({ request }) =>
    handle(request, { feature: 'selectors' }, async (caller) => {
      const input = validateSelectorInput(await readJson(request), null)
      const selector = { id: freshId(), updated_at: isoNow(), ...input }
      commit({ selectors: [...identity().selectors, selector] })
      audit(caller, {
        kind: 'rbac_change',
        action: 'content_selector.create',
        target: selector.name,
      })
      return jsonWithEtag(selectorView(selector), etagOfSelector(selector), 201)
    }),
  ),

  http.get('/api/v1/content-selectors/:selectorId', ({ request, params }) =>
    handle(request, { feature: 'selectors' }, () => {
      const selector = findSelector(String(params.selectorId))
      return jsonWithEtag(selectorView(selector), etagOfSelector(selector))
    }),
  ),

  http.put('/api/v1/content-selectors/:selectorId', ({ request, params }) =>
    handle(request, { feature: 'selectors' }, async (caller) => {
      const selector = findSelector(String(params.selectorId))
      requireIfMatch(request, etagOfSelector(selector))
      const input = validateSelectorInput(await readJson(request), selector.id)
      const next = { ...selector, ...input, updated_at: isoNow() }
      commit({ selectors: identity().selectors.map((s) => (s.id === selector.id ? next : s)) })
      audit(caller, { kind: 'rbac_change', action: 'content_selector.update', target: next.name })
      return jsonWithEtag(selectorView(next), etagOfSelector(next))
    }),
  ),

  http.delete('/api/v1/content-selectors/:selectorId', ({ request, params }) =>
    handle(request, { feature: 'selectors' }, (caller) => {
      const selector = findSelector(String(params.selectorId))
      checkOptionalIfMatch(request, etagOfSelector(selector))
      const users = selectorView(selector).used_by_privileges
      if (users.length > 0) {
        throw new MockProblem(
          409,
          'resource_in_use',
          'The content selector is used by privileges',
          {
            details: {
              dependents: users.map((p) => ({ kind: 'privilege', id: p.id, name: p.name })),
            },
          },
        )
      }
      commit({ selectors: identity().selectors.filter((s) => s.id !== selector.id) })
      audit(caller, {
        kind: 'rbac_change',
        action: 'content_selector.delete',
        target: selector.name,
      })
      return noContent()
    }),
  ),
]
