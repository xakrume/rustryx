import { http, HttpResponse, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { lastRunSummary, POLICY_IDS, startDryRun } from '@/features/lifecycle/mocks'
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

type Policy = components['schemas']['CleanupPolicy']
type PolicyView = components['schemas']['CleanupPolicyView']
type PolicyInput = components['schemas']['CleanupPolicyInput']
type FormatInfo = components['schemas']['FormatInfo']
type Criteria = components['schemas']['CleanupCriteria']
type RepoFormat = components['schemas']['RepoFormat']

const API = '/api/v1'
const KIND = 'cleanup-policy'

interface PolicyRecord {
  policy: Policy
  version: number
  repositories: readonly string[]
}

const none: Criteria = {
  older_than_days: null,
  last_downloaded_days: null,
  keep_last_versions: null,
  name_pattern: null,
  release_type: null,
}

function record(
  id: string,
  name: string,
  description: string,
  format: RepoFormat,
  criteria: Partial<Criteria>,
  repos: readonly string[],
  age: number,
): PolicyRecord {
  return {
    policy: {
      id,
      name,
      description,
      format,
      criteria: { ...none, ...criteria },
      created_at: daysAgo(age + 30),
      updated_at: daysAgo(age),
    },
    version: 1,
    repositories: repos,
  }
}

function seed(): PolicyRecord[] {
  return [
    record(
      POLICY_IDS.maven,
      'maven-old-snapshots',
      'Drop snapshots nobody resolves any more.',
      'maven2',
      { older_than_days: 90, release_type: 'prerelease' },
      ['maven-snapshots'],
      3,
    ),
    record(
      POLICY_IDS.npm,
      'npm-stale-packages',
      'Keep ten versions of each package, drop the unused rest.',
      'npm',
      { last_downloaded_days: 180, keep_last_versions: 10 },
      ['npm-snapshots', 'npm-releases'],
      9,
    ),
    record(
      POLICY_IDS.docker,
      'docker-tag-cleanup',
      'CI tags older than a month.',
      'docker',
      { older_than_days: 30, name_pattern: '^ci-.*' },
      ['docker-snapshots'],
      12,
    ),
    record(
      POLICY_IDS.pypi,
      'pypi-old-dev-builds',
      'Development builds of internal packages.',
      'pypi',
      { older_than_days: 14, name_pattern: '.*\\.dev[0-9]+$' },
      ['pypi-snapshots'],
      20,
    ),
    record(
      POLICY_IDS.raw,
      'raw-temp-files',
      'Scratch uploads.',
      'raw',
      { older_than_days: 7 },
      [],
      2,
    ),
    record(POLICY_IDS.helm, 'helm-old-charts', '', 'helm', { older_than_days: 365 }, [], 40),
  ]
}

let state: readonly PolicyRecord[] = seed()
let counter = 0

export function resetCleanupMock(): void {
  state = seed()
  counter = 0
}

const find = (id: string) => state.find((r) => r.policy.id === id)
const notFound = () => problem(404, 'cleanup_policy_not_found', 'Cleanup policy not found')

function view(r: PolicyRecord): PolicyView {
  return {
    ...r.policy,
    attached_repositories: repositories
      .filter((repo) => r.repositories.includes(repo.name))
      .map((repo) => ({
        id: repo.id,
        name: repo.name,
        format: repo.format,
        repo_type: repo.repo_type,
      })),
    last_run: lastRunSummary(r.policy.id),
  }
}

const BASE = [
  'older_than_days',
  'last_downloaded_days',
  'keep_last_versions',
  'name_pattern',
  'release_type',
] as const satisfies readonly (keyof Criteria)[]

function formatInfo(
  format: RepoFormat,
  label: string,
  criteria: readonly FormatInfo['cleanup_criteria'][number][],
  hosted = true,
): FormatInfo {
  return {
    format,
    label,
    supports: { hosted, proxy: true, group: true },
    default_write_policy: 'allow_once',
    uploadable_in_ui: true,
    cleanup_criteria: [...criteria],
    config_schema: { type: 'object', properties: {} },
    client_ids: [],
  }
}

const FORMATS: readonly FormatInfo[] = [
  formatInfo('maven2', 'Maven 2', BASE),
  formatInfo('npm', 'npm', [
    'older_than_days',
    'last_downloaded_days',
    'keep_last_versions',
    'name_pattern',
  ]),
  formatInfo('pypi', 'PyPI', [
    'older_than_days',
    'last_downloaded_days',
    'keep_last_versions',
    'name_pattern',
  ]),
  formatInfo('docker', 'Docker', ['older_than_days', 'last_downloaded_days', 'name_pattern']),
  formatInfo('helm', 'Helm', ['older_than_days', 'last_downloaded_days', 'keep_last_versions']),
  formatInfo('raw', 'Raw', ['older_than_days', 'name_pattern']),
  formatInfo('pub', 'Dart pub', ['older_than_days', 'last_downloaded_days', 'keep_last_versions']),
]

/** The linear time engine has no backreferences or lookaround: the mock refuses them like the server. */
const UNSUPPORTED_PATTERN = /\\[1-9]|\(\?[=!<]/

function validate(input: PolicyInput, ignoreId?: string): Response | null {
  const fields: { field: string; code: string; message: string }[] = []
  const info = FORMATS.find((f) => f.format === input.format)
  const set = (Object.keys(input.criteria) as (keyof Criteria)[]).filter(
    (k) => input.criteria[k] !== null,
  )
  if (set.length === 0) {
    fields.push({ field: '/criteria', code: 'invalid', message: 'Set at least one criterion' })
  }
  for (const key of set) {
    if (info !== undefined && !info.cleanup_criteria.includes(key)) {
      fields.push({
        field: `/criteria/${key}`,
        code: 'unsupported_for_format',
        message: `Not supported for ${info.label} repositories`,
      })
    }
  }
  const pattern = input.criteria.name_pattern
  if (pattern !== null && UNSUPPORTED_PATTERN.test(pattern)) {
    return problem(
      400,
      'invalid_pattern',
      'The pattern uses a construct the engine does not support',
      {
        details: {
          fields: [
            {
              field: '/criteria/name_pattern',
              code: 'invalid_pattern',
              message: 'Backreferences and lookaround are not supported',
            },
          ],
        },
      },
    )
  }
  const duplicate = state.find((r) => r.policy.name === input.name && r.policy.id !== ignoreId)
  if (duplicate !== undefined) {
    return problem(409, 'already_exists', 'A cleanup policy with this name already exists')
  }
  return fields.length === 0
    ? null
    : problem(400, 'invalid_input', 'The request was not accepted', { details: { fields } })
}

export const handlers: HttpHandler[] = [
  http.get(`${API}/formats`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    return json({ items: FORMATS, next: null })
  }),

  http.get(`${API}/cleanup-policies`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const url = new URL(request.url)
    const q = url.searchParams.get('q')?.toLowerCase()
    const format = url.searchParams.get('format')
    const rows = state
      .filter(
        (r) =>
          (q === undefined || r.policy.name.toLowerCase().startsWith(q)) &&
          (format === null || r.policy.format === format),
      )
      .map(view)
    const page = pageOf(rows, url)
    return page instanceof Response ? page : json(page)
  }),

  http.post(`${API}/cleanup-policies`, async ({ request }) => {
    const refused = await gate(request)
    if (refused) return refused
    const input = await readJsonBody<PolicyInput>(request)
    if (input === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    const rejected = validate(input)
    if (rejected) return rejected
    counter += 1
    const created: PolicyRecord = {
      policy: {
        id: mockId(2, 100 + counter),
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
        ETag: etagOf(KIND, created.policy.id, 1),
        Location: `${API}/cleanup-policies/${created.policy.id}`,
      },
    })
  }),

  http.get(`${API}/cleanup-policies/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    return json(view(found), { headers: { ETag: etagOf(KIND, found.policy.id, found.version) } })
  }),

  http.put(`${API}/cleanup-policies/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    const stale = checkIfMatch(request, etagOf(KIND, found.policy.id, found.version))
    if (stale) return stale
    const input = await readJsonBody<PolicyInput>(request)
    if (input === null) return problem(400, 'invalid_json', 'The request body is not valid JSON')
    if (input.format !== found.policy.format && found.repositories.length > 0) {
      return problem(
        400,
        'immutable_field',
        'The format cannot change while repositories use the policy',
        {
          details: {
            fields: [{ field: '/format', code: 'immutable', message: 'In use by repositories' }],
          },
        },
      )
    }
    const rejected = validate(input, found.policy.id)
    if (rejected) return rejected
    const next: PolicyRecord = {
      ...found,
      version: found.version + 1,
      policy: { ...found.policy, ...input, updated_at: hoursAgo(0) },
    }
    state = state.map((r) => (r.policy.id === next.policy.id ? next : r))
    return json(view(next), { headers: { ETag: etagOf(KIND, next.policy.id, next.version) } })
  }),

  http.delete(`${API}/cleanup-policies/:id`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    const force = new URL(request.url).searchParams.get('force') === 'true'
    if (found.repositories.length > 0 && !force) {
      return problem(409, 'resource_in_use', 'Repositories still use this policy', {
        details: {
          dependents: found.repositories.map((name) => ({ kind: 'repository', id: null, name })),
        },
      })
    }
    state = state.filter((r) => r.policy.id !== found.policy.id)
    return new HttpResponse(null, { status: 204 })
  }),

  http.post(`${API}/cleanup-policies/:id/actions/preview`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const found = find(String(params.id))
    if (found === undefined) return notFound()
    const running = lastRunSummary(found.policy.id)
    if (running !== null && (running.status === 'queued' || running.status === 'running')) {
      return problem(409, 'run_in_progress', 'A dry run of this policy is already running')
    }
    const run = startDryRun({
      id: found.policy.id,
      name: found.policy.name,
      criteria: found.policy.criteria,
      repositories: found.repositories,
    })
    return json(run, {
      status: 202,
      headers: { Location: `${API}/lifecycle-runs/${run.id}`, 'Retry-After': '2' },
    })
  }),
]
