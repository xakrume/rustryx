import { http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import {
  checkIfMatch,
  etagOf,
  gate,
  hoursAgo,
  mockId,
  readJsonBody,
} from '@/features/system/mock-kit'
import { problem } from '@/mocks/problem'
import { json } from '@/mocks/store'

type Section = components['schemas']['SettingsSection']
type SectionName = Section['section']
type Source = Section['sources'][string]

const API = '/api/v1'
const KIND = 'settings'

interface Spec {
  type: 'integer' | 'boolean' | 'string'
  title: string
  description: string
  minimum?: number
  maximum?: number
  value: unknown
  source: Source
  editable: boolean
}

const SECTIONS: Record<string, Record<string, Spec>> = {
  general: {
    base_url: {
      type: 'string',
      title: 'Base URL',
      description: 'The address users reach the server at. Client snippets and links use it.',
      value: 'https://repo.example.org',
      source: 'config',
      editable: false,
    },
    read_only_mode: {
      type: 'boolean',
      title: 'Read only mode',
      description:
        'Refuse every write while an operation such as a migration runs. Reads keep working.',
      value: false,
      source: 'default',
      editable: true,
    },
  },
  security: {
    session_idle_timeout_secs: {
      type: 'integer',
      title: 'Session idle timeout (seconds)',
      description: 'A session ends after this long without activity.',
      value: 1800,
      source: 'config',
      editable: false,
    },
    session_absolute_timeout_secs: {
      type: 'integer',
      title: 'Session absolute lifetime (seconds)',
      description: 'A session ends this long after sign in, however active it is.',
      value: 28800,
      source: 'config',
      editable: false,
    },
    require_token_expiry: {
      type: 'boolean',
      title: 'Require an expiry on tokens',
      description: 'Tokens without an expiry date are refused.',
      value: true,
      source: 'config',
      editable: false,
    },
    smtp_password_set: {
      type: 'boolean',
      title: 'Mail password',
      description: 'Whether a password for the mail server is configured.',
      value: true,
      source: 'config',
      editable: false,
    },
    token_default_ttl_days: {
      type: 'integer',
      title: 'Default token lifetime (days)',
      description: 'Used when a new token does not name its own lifetime.',
      minimum: 1,
      maximum: 365,
      value: 90,
      source: 'default',
      editable: true,
    },
    token_max_ttl_days: {
      type: 'integer',
      title: 'Longest token lifetime (days)',
      description: 'No token can live longer than this.',
      minimum: 1,
      maximum: 365,
      value: 365,
      source: 'default',
      editable: true,
    },
  },
  storage: {
    last_downloaded_update_interval_secs: {
      type: 'integer',
      title: 'Last download update interval (seconds)',
      description: 'How often download times are written. Cleanup by last download reads them.',
      minimum: 60,
      maximum: 86400,
      value: 3600,
      source: 'default',
      editable: true,
    },
    quota_warning_percent: {
      type: 'integer',
      title: 'Quota warning level (percent)',
      description: 'A blob store shows "near quota" from this share of its soft quota.',
      minimum: 1,
      maximum: 100,
      value: 80,
      source: 'runtime',
      editable: true,
    },
  },
}

interface SectionState {
  values: Record<string, unknown>
  sources: Record<string, Source>
  version: number
  updatedAt: string | null
}

function seed(): Record<string, SectionState> {
  return Object.fromEntries(
    Object.entries(SECTIONS).map(([name, fields]) => [
      name,
      {
        values: Object.fromEntries(Object.entries(fields).map(([k, f]) => [k, f.value])),
        sources: Object.fromEntries(Object.entries(fields).map(([k, f]) => [k, f.source])),
        version: 1,
        updatedAt: null,
      },
    ]),
  )
}

let state: Readonly<Record<string, SectionState>> = seed()

export function resetSettingsMock(): void {
  state = seed()
}

function view(name: string): Section | undefined {
  const fields = SECTIONS[name]
  const current = state[name]
  if (fields === undefined || current === undefined) return undefined
  return {
    section: name as SectionName,
    values: current.values,
    sources: current.sources,
    editable_fields: Object.entries(fields)
      .filter(([, f]) => f.editable)
      .map(([k]) => k),
    updated_at: current.updatedAt,
    updated_by:
      current.updatedAt === null
        ? null
        : { id: mockId(0, 1), username: 'admin', display_name: 'Administrator', source: 'local' },
  }
}

const jsonSchema = () => ({
  sections: Object.fromEntries(
    Object.entries(SECTIONS).map(([name, fields]) => [
      name,
      {
        type: 'object',
        properties: Object.fromEntries(
          Object.entries(fields).map(([key, f]) => [
            key,
            {
              type: f.type,
              title: f.title,
              description: f.description,
              ...(f.minimum === undefined ? {} : { minimum: f.minimum }),
              ...(f.maximum === undefined ? {} : { maximum: f.maximum }),
            },
          ]),
        ),
      },
    ]),
  ),
})

const notFound = () => problem(404, 'not_found', 'Settings section not found')

export const handlers: HttpHandler[] = [
  http.get(`${API}/settings/schema`, async ({ request }) => {
    const refused = await gate(request)
    return refused ?? json(jsonSchema())
  }),

  http.get(`${API}/settings/:section`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const name = String(params.section)
    const body = view(name)
    if (body === undefined) return notFound()
    return json(body, { headers: { ETag: etagOf(KIND, name, state[name]?.version ?? 1) } })
  }),

  http.put(`${API}/settings/:section`, async ({ request, params }) => {
    const refused = await gate(request)
    if (refused) return refused
    const name = String(params.section)
    const fields = SECTIONS[name]
    const current = state[name]
    if (fields === undefined || current === undefined) return notFound()
    const stale = checkIfMatch(request, etagOf(KIND, name, current.version))
    if (stale) return stale
    const body = await readJsonBody<{ values?: Record<string, unknown> }>(request)
    if (body === null || typeof body.values !== 'object' || body.values === null) {
      return problem(400, 'invalid_json', 'The request body is not valid JSON')
    }
    const entries = Object.entries(body.values)
    const managed = entries.find(([key]) => fields[key]?.editable !== true)
    if (managed !== undefined) {
      return problem(
        409,
        'setting_managed_by_config',
        `${managed[0]} comes from the configuration and cannot change at runtime`,
      )
    }
    const issues: { field: string; code: string; message: string }[] = []
    for (const [key, value] of entries) {
      const spec = fields[key]
      if (spec === undefined) continue
      if (spec.type === 'integer') {
        const n = Number(value)
        if (
          !Number.isInteger(n) ||
          (spec.minimum !== undefined && n < spec.minimum) ||
          (spec.maximum !== undefined && n > spec.maximum)
        ) {
          issues.push({
            field: `/values/${key}`,
            code: 'invalid',
            message: `Must be a whole number from ${spec.minimum} to ${spec.maximum}`,
          })
        }
      }
    }
    const merged = { ...current.values, ...body.values }
    if (Number(merged.token_default_ttl_days) > Number(merged.token_max_ttl_days)) {
      issues.push({
        field: '/values/token_default_ttl_days',
        code: 'invalid',
        message: 'The default lifetime must not exceed the longest lifetime',
      })
    }
    if (issues.length > 0) {
      return problem(400, 'invalid_input', 'The request was not accepted', {
        details: { fields: issues },
      })
    }
    const next: SectionState = {
      ...current,
      values: merged,
      sources: {
        ...current.sources,
        ...Object.fromEntries(entries.map(([k]) => [k, 'runtime'])),
      },
      version: current.version + 1,
      updatedAt: hoursAgo(0),
    }
    state = { ...state, [name]: next }
    return json(view(name), { headers: { ETag: etagOf(KIND, name, next.version) } })
  }),
]
