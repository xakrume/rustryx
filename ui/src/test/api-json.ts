/**
 * Shape of the JSON objects the mock contract tests read.
 * One interface covers every object in those bodies, so a field that a
 * particular response does not have is simply never read.
 */
export interface ApiJson {
  action: string
  actions: string[]
  actor_name: string
  allow_anonymous: boolean
  anonymous_effective: boolean
  anonymous_role: ApiJson
  assets: ApiJson[]
  at: string
  auth: ApiJson | null
  builtin: boolean
  cleanup_policies: ApiJson[]
  code: string
  compile_errors: ApiJson[]
  component: ApiJson
  component_count: number
  denied: boolean
  dependents: ApiJson[]
  details: ApiJson
  display_prefix: string
  editable_fields: string[]
  effective_repository_count: number
  enabled: boolean
  error: ApiJson
  errors: ApiJson[]
  external_group: string
  field: string
  fields: ApiJson[]
  format_config: ApiJson
  global_editable: boolean
  global_enabled: boolean
  grant: ApiJson
  groups: string[]
  groups_confirmed_at: string | null
  id: string
  invalidated_at: string
  items: ApiJson[]
  kind: string
  limits: ApiJson
  max_days: number
  max_expression_bytes: number
  message: string
  name: string
  never_used: boolean
  next: string | null
  offset: number
  online: boolean
  origin: string
  outcome: string
  path: string
  permissions: ApiJson
  policy: string
  priority: number
  proxy: ApiJson
  public_repository_count: number
  read_only_reason: string
  reason: string
  replaced: string[]
  repository: string
  request_id: string
  result: string
  results: ApiJson[]
  revoked_at: string | null
  role: ApiJson
  roles: ApiJson[]
  routing_rule: ApiJson
  secret: string
  source: string
  state: string
  stats: ApiJson | null
  target: string | null
  tokens_valid_after: string | null
  unmatched_groups: string[]
  updated: number
  uploadable: boolean
  upstream: ApiJson
  username: string
  valid: boolean
  via: string
}

/**
 * `JSON.parse` is typed as `any`. The cast below is the boundary: the value
 * is checked to be a non-null object, and the contract tests assert the
 * fields they care about instead of re-validating the whole document.
 */
export function parseJson(text: string): ApiJson {
  const value: unknown = JSON.parse(text)
  if (typeof value === 'object' && value !== null) return value as ApiJson
  throw new Error('expected a JSON object')
}

/**
 * An empty HTTP body is null at runtime. Callers that read fields only do
 * so after checking the status; reading one here still throws.
 */
export function emptyJson(): ApiJson {
  return null as unknown as ApiJson
}

/** Index access that `noUncheckedIndexedAccess` would otherwise leave undefined. */
export function nth<T>(items: readonly T[], index: number): T {
  const item = items[index]
  if (item === undefined) throw new Error(`missing item ${index}`)
  return item
}
