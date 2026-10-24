import { describe, expect, it } from 'vitest'
import { ApiError } from '@/api/errors'
import { configFields, configToJson, configValuesFrom, humanize } from './format-config'
import {
  applyServerErrors,
  defaultValues,
  repositoryFormSchema,
  toCreateRequest,
  toPatch,
  valuesFromRepository,
  type RepositoryFormValues,
} from './values'
import { buildRepositories } from '@/mocks/data'

const MAVEN_SCHEMA = {
  type: 'object',
  properties: {
    version_policy: { title: 'Version policy', enum: ['release', 'snapshot'], default: 'release' },
    strict: { type: 'boolean', default: true },
    limit: { type: 'integer', default: 5 },
    note: { type: 'string' },
    nested: { type: 'object', properties: {} },
  },
}

describe('format options from a JSON schema', () => {
  const fields = configFields(MAVEN_SCHEMA)

  it('understands flat enum, boolean, integer and string properties and skips the rest', () => {
    expect(fields.map((f) => [f.key, f.kind])).toEqual([
      ['version_policy', 'enum'],
      ['strict', 'boolean'],
      ['limit', 'integer'],
      ['note', 'string'],
    ])
    expect(fields[0]).toMatchObject({ label: 'Version policy', initial: 'release' })
    expect(fields[1]?.label).toBe('Strict')
  })

  it('gives nothing for a schema without properties', () => {
    expect(configFields(undefined)).toEqual([])
    expect(configFields({ type: 'object' })).toEqual([])
    expect(configFields(null)).toEqual([])
  })

  it('lets stored values win over defaults and turns the form values back into JSON', () => {
    const values = configValuesFrom(fields, { version_policy: 'snapshot', limit: 9, strict: false })
    expect(values).toEqual({ version_policy: 'snapshot', strict: false, limit: '9', note: '' })
    expect(configToJson(fields, { ...values, limit: '12', note: '' })).toEqual({
      version_policy: 'snapshot',
      strict: false,
      limit: 12,
    })
    expect(configToJson(fields, { limit: 'x' })).toEqual({})
  })

  it('writes a key name for people', () => {
    expect(humanize('layout_policy')).toBe('Layout policy')
    expect(humanize('a-b')).toBe('A b')
  })
})

describe('repository form values', () => {
  const create = repositoryFormSchema('create')
  const valid: RepositoryFormValues = {
    ...defaultValues([]),
    name: 'maven-new',
    format: 'maven2',
    blob_store_id: '0192f6c4-5a1e-7000-8000-000000000029',
  }

  it('accepts a complete hosted repository', () => {
    expect(create.safeParse(valid).success).toBe(true)
  })

  it('rejects names the contract rejects', () => {
    for (const name of ['', '-x', 'a..b', 'ends.', 'has space', 'x'.repeat(129)]) {
      const result = create.safeParse({ ...valid, name })
      expect(result.success, name).toBe(false)
    }
  })

  it('checks the numbers and the URL of a proxy and the members of a group', () => {
    const proxy = {
      ...valid,
      repo_type: 'proxy' as const,
      remote_url: 'ftp://x',
      max_concurrent_upstream: '0',
    }
    const issues = create.safeParse(proxy)
    expect(issues.success).toBe(false)
    const paths = issues.success ? [] : issues.error.issues.map((i) => i.path[0])
    expect(paths).toEqual(expect.arrayContaining(['remote_url', 'max_concurrent_upstream']))
    expect(create.safeParse({ ...valid, repo_type: 'group' }).success).toBe(false)
    expect(create.safeParse({ ...valid, quota_gib: 'ten' }).success).toBe(false)
    expect(
      create.safeParse({
        ...valid,
        repo_type: 'proxy',
        remote_url: 'https://x.org',
        auth_password: 'p',
      }).success,
    ).toBe(false)
  })

  it('does not validate identity in edit mode', () => {
    expect(repositoryFormSchema('edit').safeParse({ ...valid, name: '', format: '' }).success).toBe(
      true,
    )
  })

  it('builds the create request for each type', () => {
    const fields = configFields(MAVEN_SCHEMA)
    const hosted = toCreateRequest({ ...valid, quota_gib: '1.5', config: { limit: '3' } }, fields)
    expect(hosted).toMatchObject({
      name: 'maven-new',
      write_policy: 'allow_once',
      quota_bytes: 1.5 * 1024 ** 3,
    })
    expect(hosted.format_config).toEqual({ limit: 3 })
    expect(hosted.proxy).toBeUndefined()

    const proxy = toCreateRequest(
      {
        ...valid,
        repo_type: 'proxy',
        remote_url: ' https://registry.example.org ',
        auth_username: 'u',
        auth_password: 'p',
        routing_rule_id: 'rule-1',
      },
      [],
    )
    expect(proxy.write_policy).toBeUndefined()
    expect(proxy.routing_rule_id).toBe('rule-1')
    expect(proxy.proxy).toMatchObject({
      remote_url: 'https://registry.example.org',
      auth: { username: 'u', password: 'p' },
    })

    const group = toCreateRequest({ ...valid, repo_type: 'group', members: ['a', 'b'] }, [])
    expect(group.group).toEqual({ members: ['a', 'b'] })
    expect(
      toCreateRequest({ ...valid, repo_type: 'proxy', remote_url: 'https://x.org' }, []).proxy
        ?.auth,
    ).toBeNull()
  })

  it('builds a patch of only what changed', () => {
    const repo = buildRepositories().find((r) => r.name === 'npm-proxy')!
    const initial = valuesFromRepository(repo, [])
    expect(toPatch(initial, initial, [])).toEqual({})
    const changed: RepositoryFormValues = {
      ...initial,
      allow_anonymous: !initial.allow_anonymous,
      online: false,
      quota_gib: '2',
      cleanup_policy_ids: ['p1'],
      routing_rule_id: 'r1',
      remote_url: 'https://other.example.org',
      content_max_age_secs: '10',
      metadata_max_age_secs: '20',
      negative_cache_ttl_secs: '30',
      max_concurrent_upstream: '4',
      auth_username: 'bob',
      auth_password: 'pw',
    }
    expect(toPatch(changed, initial, [])).toEqual({
      allow_anonymous: !initial.allow_anonymous,
      online: false,
      quota_bytes: 2 * 1024 ** 3,
      cleanup_policy_ids: ['p1'],
      routing_rule_id: 'r1',
      proxy: {
        remote_url: 'https://other.example.org',
        content_max_age_secs: 10,
        metadata_max_age_secs: 20,
        negative_cache_ttl_secs: 30,
        max_concurrent_upstream: 4,
        auth: { username: 'bob', password: 'pw' },
      },
    })
    expect(
      toPatch(
        { ...initial, routing_rule_id: '', remove_credentials: true },
        { ...initial, routing_rule_id: 'x' },
        [],
      ),
    ).toMatchObject({ routing_rule_id: null, proxy: { auth: null } })
  })

  it('patches the write policy of a hosted repository and the members of a group', () => {
    const hosted = valuesFromRepository(
      buildRepositories().find((r) => r.name === 'npm-releases')!,
      [],
    )
    expect(toPatch({ ...hosted, write_policy: 'deny' }, hosted, [])).toEqual({
      write_policy: 'deny',
    })
    const group = valuesFromRepository(
      buildRepositories().find((r) => r.name === 'npm-group')!,
      [],
    )
    expect(toPatch({ ...group, members: ['b', 'a'] }, group, [])).toEqual({
      group: { members: ['b', 'a'] },
    })
  })
})

describe('server errors on the form', () => {
  function fakeForm() {
    const calls: { name: string; message: string; focus: boolean }[] = []
    return {
      calls,
      setError: (name: string, error: { message?: string }, options?: { shouldFocus?: boolean }) =>
        calls.push({ name, message: error.message ?? '', focus: options?.shouldFocus === true }),
    }
  }
  const error = (code: string, details: Record<string, unknown> | null, status = 400) =>
    new ApiError({ code, message: 'm', status, details })

  it('maps JSON pointers onto the flat fields and reports the first step with an error', () => {
    const form = fakeForm()
    const result = applyServerErrors(
      form as never,
      error('invalid_input', {
        fields: [
          { field: '/proxy/remote_url', code: 'x', message: 'bad url' },
          { field: '/group/members/1', code: 'x', message: 'bad member' },
          { field: '/quota_bytes', code: 'x', message: 'bad quota' },
          { field: '/format_config/version_policy', code: 'x', message: 'bad option' },
          { field: '/unknown', code: 'x', message: 'ignored' },
        ],
      }),
    )
    expect(form.calls.map((c) => c.name)).toEqual(['remote_url', 'members', 'quota_gib', 'config'])
    expect(form.calls[0]?.focus).toBe(true)
    expect(form.calls[1]?.focus).toBe(false)
    expect(result).toEqual({ applied: true, step: 2 })
  })

  it('sends a name clash back to step one and an SSRF refusal to the URL with its rule', () => {
    const taken = fakeForm()
    expect(applyServerErrors(taken as never, error('already_exists', null, 409))).toEqual({
      applied: true,
      step: 1,
    })
    expect(taken.calls[0]).toMatchObject({ name: 'name' })
    const ssrf = fakeForm()
    applyServerErrors(ssrf as never, error('upstream_url_rejected', { rule: 'private_address' }))
    expect(ssrf.calls[0]?.message).toContain('private_address')
    const plain = fakeForm()
    applyServerErrors(plain as never, error('upstream_url_rejected', null))
    expect(plain.calls[0]?.message).toBe('The server refused this address.')
  })

  it('does nothing for an error it cannot place', () => {
    const form = fakeForm()
    expect(applyServerErrors(form as never, error('internal_error', null, 500))).toEqual({
      applied: false,
      step: null,
    })
    expect(applyServerErrors(form as never, new Error('boom'))).toEqual({
      applied: false,
      step: null,
    })
    expect(form.calls).toEqual([])
  })
})
