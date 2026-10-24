import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import {
  checkIfMatch,
  daysAgo,
  etagOf,
  gate,
  hoursAgo,
  mockId,
  pageOf,
  readJsonBody,
} from '@/features/system/mock-kit'
import { problem } from '@/mocks/problem'
import { json, repositories } from '@/mocks/store'

type Rule = components['schemas']['RoutingRule']
type RuleView = components['schemas']['RoutingRuleView']
type RuleInput = components['schemas']['RoutingRuleInput']
type PatternIssue = components['schemas']['RoutingPatternIssue']
type TestItem = components['schemas']['RoutingTestResult']['items'][number]

const API = '/api/v1'
const KIND = 'routing-rule'
const MAX_TEST_PATHS = 100

interface RuleRecord {
  rule: Rule
  version: number
  repositories: readonly string[]
}

function record(
  n: number,
  name: string,
  description: string,
  mode: Rule['mode'],
  patterns: string[],
  repos: readonly string[],
): RuleRecord {
  return {
    rule: {
      id: mockId(9, n),
      name,
      description,
      mode,
      patterns,
      created_at: daysAgo(40 + n),
      updated_at: daysAgo(n),
    },
    version: 1,
    repositories: repos,
  }
}

function seed(): RuleRecord[] {
  return [
    record(
      1,
      'block-internal-paths',
      'Never fetch internal paths from public upstreams.',
      'block',
      ['^/internal/.*', '^/private-.*'],
      ['maven-proxy', 'npm-proxy'],
    ),
    record(
      2,
      'allow-corporate-groups',
      'Only our own group ids come from the mirror.',
      'allow',
      ['^/com/example/.*', '^/org/internal/.*'],
      ['pypi-proxy'],
    ),
    record(3, 'block-snapshots-from-central', '', 'block', ['.*-SNAPSHOT.*'], []),
    record(
      4,
      'block-executables',
      'No binaries through the raw and helm proxies.',
      'block',
      ['.*\\.(exe|dll|msi)$'],
      ['raw-proxy', 'helm-proxy', 'docker-proxy'],
    ),
  ]
}

let state: readonly RuleRecord[] = seed()
let counter = 0

export function resetRoutingMock(): void {
  state = seed()
  counter = 0
}

const find = (id: string) => state.find((r) => r.rule.id === id)
const notFound = () => problem(404, 'routing_rule_not_found', 'Routing rule not found')

function view(r: RuleRecord): RuleView {
  return {
    ...r.rule,
    assigned_repositories: repositories
      .filter((repo) => r.repositories.includes(repo.name))
      .map((repo) => ({
        id: repo.id,
        name: repo.name,
        format: repo.format,
        repo_type: repo.repo_type,
      })),
  }
}

/** The engine is linear time: no backreferences and no lookaround, like the server's. */
function lint(patterns: readonly string[]): PatternIssue[] {
  return patterns.flatMap((pattern, index): PatternIssue[] => {
    if (pattern.trim() === '') return [{ index, offset: 0, message: 'The pattern is empty' }]
    const unsupported = /\\[1-9]|\(\?<?[=!]/.exec(pattern)
    if (unsupported !== null) {
      return [
        {
          index,
          offset: unsupported.index,
          message: 'Backreferences and lookaround are not supported',
        },
      ]
    }
    try {
      new RegExp(pattern)
      return []
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Invalid regular expression'
      return [
        { index, offset: null, message: message.replace(/^Invalid regular expression: /, '') },
      ]
    }
  })
}

function evaluate(
  mode: Rule['mode'],
  patterns: readonly string[],
  paths: readonly string[],
): TestItem[] {
  const compiled = patterns.map((p) => ({ source: p, regex: new RegExp(`^(?:${p})$`) }))
  return paths.map((path) => {
    const hit = compiled.find((c) => c.regex.test(path))
    if (mode === 'block') {
      return hit
        ? {
            path,
            outcome: 'blocked',
            matched_pattern: hit.source,
            reason: 'matches a block pattern',
          }
        : { path, outcome: 'allowed', matched_pattern: null, reason: 'matches no block pattern' }
    }
    return hit
      ? {
          path,
          outcome: 'allowed',
          matched_pattern: hit.source,
          reason: 'matches an allow pattern',
        }
      : { path, outcome: 'blocked', matched_pattern: null, reason: 'matches no allow pattern' }
  })
}

function invalidPattern(issues: readonly PatternIssue[]): Response {
  return problem(400, 'invalid_pattern', 'A pattern is not supported by the engine', {
    details: {
      fields: issues.map((i) => ({
        field: `/patterns/${i.index}`,
        code: 'invalid_pattern',
        message: i.message,
      })),
    },
  })
}

export const handlers: HttpHandler[] = [
  http.get(`${API}/routing-rules`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const q = url.searchParams.get('q')?.toLowerCase()
    const rows = state
      .filter((r) => q === undefined || r.rule.name.toLowerCase().startsWith(q))
      .map(view)
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.post(`${API}/routing-rules/validate`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const body = await readJsonBody<{ patterns?: string[] }>(request)
    if (body === null || !Array.isArray(body.patterns)) {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    const errors = lint(body.patterns)
    return json({ valid: errors.length === 0, errors })
  }),

  http.post(`${API}/routing-rules/test`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const body = await readJsonBody<{ mode?: Rule['mode']; patterns?: string[]; paths?: string[] }>(
      request,
    )
    if (
      body === null ||
      !Array.isArray(body.patterns) ||
      !Array.isArray(body.paths) ||
      !body.mode
    ) {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    if (body.paths.length > MAX_TEST_PATHS) {
      return problem(400, 'invalid_query', `Test at most ${MAX_TEST_PATHS} paths at a time`)
    }
    const issues = lint(body.patterns)
    if (issues.length > 0) return invalidPattern(issues)
    return json({ items: evaluate(body.mode, body.patterns, body.paths) })
  }),

  http.post(`${API}/routing-rules`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const input = await readJsonBody<RuleInput>(request)
    if (input === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    const issues = lint(input.patterns)
    if (issues.length > 0) return invalidPattern(issues)
    if (state.some((r) => r.rule.name === input.name)) {
      return problem(409, 'already_exists', 'A routing rule with this name already exists')
    }
    counter += 1
    const created: RuleRecord = {
      rule: {
        id: mockId(9, 100 + counter),
        ...input,
        created_at: hoursAgo(0),
        updated_at: hoursAgo(0),
      },
      version: 1,
      repositories: [],
    }
    state = [...state, created]
    return json(view(created), {
      status: 201,
      headers: {
        ETag: etagOf(KIND, created.rule.id, 1),
        Location: `${API}/routing-rules/${created.rule.id}`,
      },
    })
  }),

  http.get(`${API}/routing-rules/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    return json(view(found), { headers: { ETag: etagOf(KIND, found.rule.id, found.version) } })
  }),

  http.put(`${API}/routing-rules/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    const stale = checkIfMatch(request, etagOf(KIND, found.rule.id, found.version))
    if (stale) return stale
    const input = await readJsonBody<RuleInput>(request)
    if (input === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    const issues = lint(input.patterns)
    if (issues.length > 0) return invalidPattern(issues)
    if (state.some((r) => r.rule.name === input.name && r.rule.id !== found.rule.id)) {
      return problem(409, 'already_exists', 'A routing rule with this name already exists')
    }
    const next: RuleRecord = {
      ...found,
      version: found.version + 1,
      rule: { ...found.rule, ...input, updated_at: hoursAgo(0) },
    }
    state = state.map((r) => (r.rule.id === next.rule.id ? next : r))
    return json(view(next), { headers: { ETag: etagOf(KIND, next.rule.id, next.version) } })
  }),

  http.delete(`${API}/routing-rules/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    if (found.repositories.length > 0) {
      return problem(409, 'resource_in_use', 'Repositories still use this routing rule', {
        details: {
          dependents: found.repositories.map((name) => ({ kind: 'repository', id: null, name })),
        },
      })
    }
    state = state.filter((r) => r.rule.id !== found.rule.id)
    return new HttpResponse(null, { status: 204 })
  }),

  http.post(`${API}/routing-rules/:id/test`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    const body = await readJsonBody<{ paths?: string[] }>(request)
    if (body === null || !Array.isArray(body.paths)) {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    return json({ items: evaluate(found.rule.mode, found.rule.patterns, body.paths) })
  }),
]
