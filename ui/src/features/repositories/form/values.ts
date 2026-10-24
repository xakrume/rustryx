import type { UseFormReturn } from 'react-hook-form'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { t } from '@/i18n'
import '@/lib/zod-setup'
import type { RepositoryCreate, RepositoryPatch, RepositoryView } from '../api'
import { configToJson, configValuesFrom, type ConfigField, type ConfigValue } from './format-config'

/**
 * The repository form in the shape forms like: flat names, text for every number (an empty
 * input is "no value", a number input cannot say that), and the credentials split in a user
 * name and a password that is write only. `toCreateRequest` and `toPatch` turn it into the API
 * bodies, so the mapping between what the user edits and what is sent lives in one place.
 */
export interface RepositoryFormValues {
  name: string
  format: string
  repo_type: 'hosted' | 'proxy' | 'group'
  blob_store_id: string
  write_policy: 'allow' | 'allow_once' | 'deny'
  allow_anonymous: boolean
  online: boolean
  /** Whole or decimal GiB, empty for no quota. */
  quota_gib: string
  cleanup_policy_ids: string[]
  /** Empty for none. */
  routing_rule_id: string
  remote_url: string
  auth_username: string
  /** Write only: sent when typed, never filled from the server, never shown again. */
  auth_password: string
  /** Edit only: drop the stored credentials. */
  remove_credentials: boolean
  content_max_age_secs: string
  metadata_max_age_secs: string
  negative_cache_ttl_secs: string
  max_concurrent_upstream: string
  /** Group members in resolution order. */
  members: string[]
  config: Record<string, ConfigValue>
}

export const GIB = 1024 ** 3

const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]*$/

function validName(name: string): boolean {
  return (
    NAME_PATTERN.test(name) && !name.includes('..') && !name.endsWith('.') && name.length <= 128
  )
}

function integerIn(value: string, min: number, max: number): boolean {
  return /^\d+$/.test(value) && Number(value) >= min && Number(value) <= max
}

function validUrl(value: string): boolean {
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:'
  } catch {
    return false
  }
}

export function repositoryFormSchema(
  mode: 'create' | 'edit',
): z.ZodType<RepositoryFormValues, RepositoryFormValues> {
  return z
    .object({
      name: z.string(),
      format: z.string(),
      repo_type: z.enum(['hosted', 'proxy', 'group']),
      blob_store_id: z.string(),
      write_policy: z.enum(['allow', 'allow_once', 'deny']),
      allow_anonymous: z.boolean(),
      online: z.boolean(),
      quota_gib: z.string(),
      cleanup_policy_ids: z.array(z.string()),
      routing_rule_id: z.string(),
      remote_url: z.string(),
      auth_username: z.string(),
      auth_password: z.string(),
      remove_credentials: z.boolean(),
      content_max_age_secs: z.string(),
      metadata_max_age_secs: z.string(),
      negative_cache_ttl_secs: z.string(),
      max_concurrent_upstream: z.string(),
      members: z.array(z.string()),
      config: z.record(z.string(), z.union([z.string(), z.boolean()])),
    })
    .superRefine((v, ctx) => {
      const issue = (path: keyof RepositoryFormValues, message: string): void => {
        ctx.addIssue({ code: 'custom', path: [path], message })
      }
      if (mode === 'create') {
        if (v.name.trim() === '') issue('name', t('repoForm.errors.nameRequired'))
        else if (!validName(v.name)) issue('name', t('repoForm.errors.namePattern'))
        if (v.format === '') issue('format', t('repoForm.errors.formatRequired'))
      }
      if (v.blob_store_id === '') issue('blob_store_id', t('repoForm.errors.blobStoreRequired'))
      if (v.quota_gib !== '' && !/^\d+(\.\d+)?$/.test(v.quota_gib)) {
        issue('quota_gib', t('repoForm.errors.quotaInvalid'))
      }
      if (v.repo_type === 'group' && v.members.length === 0) {
        issue('members', t('repoForm.errors.membersRequired'))
      }
      if (v.repo_type !== 'proxy') return
      if (v.remote_url.trim() === '') issue('remote_url', t('repoForm.errors.urlRequired'))
      else if (!validUrl(v.remote_url.trim())) issue('remote_url', t('repoForm.errors.urlInvalid'))
      const secs = (
        key: 'content_max_age_secs' | 'metadata_max_age_secs' | 'negative_cache_ttl_secs',
        max: number,
      ): void => {
        if (!integerIn(v[key], 0, max)) {
          issue(key, t('repoForm.errors.secondsInvalid', { max: max.toLocaleString() }))
        }
      }
      secs('content_max_age_secs', 31_536_000)
      secs('metadata_max_age_secs', 31_536_000)
      secs('negative_cache_ttl_secs', 86_400)
      if (!integerIn(v.max_concurrent_upstream, 1, 256)) {
        issue('max_concurrent_upstream', t('repoForm.errors.concurrentInvalid'))
      }
      if (v.auth_password !== '' && v.auth_username.trim() === '') {
        issue('auth_username', t('repoForm.errors.usernameRequired'))
      }
    })
}

export function defaultValues(
  fields: readonly ConfigField[],
  base: Partial<RepositoryFormValues> = {},
): RepositoryFormValues {
  return {
    name: '',
    format: '',
    repo_type: 'hosted',
    blob_store_id: '',
    write_policy: 'allow_once',
    allow_anonymous: false,
    online: true,
    quota_gib: '',
    cleanup_policy_ids: [],
    routing_rule_id: '',
    remote_url: '',
    auth_username: '',
    auth_password: '',
    remove_credentials: false,
    content_max_age_secs: '86400',
    metadata_max_age_secs: '1800',
    negative_cache_ttl_secs: '3600',
    max_concurrent_upstream: '16',
    members: [],
    config: configValuesFrom(fields, {}),
    ...base,
  }
}

/** The values of an existing repository, for the edit form. The password is never part of them. */
export function valuesFromRepository(
  repo: RepositoryView,
  fields: readonly ConfigField[],
): RepositoryFormValues {
  return defaultValues(fields, {
    name: repo.name,
    format: repo.format,
    repo_type: repo.repo_type,
    blob_store_id: repo.blob_store_id,
    write_policy: repo.write_policy,
    allow_anonymous: repo.allow_anonymous,
    online: repo.online,
    quota_gib: repo.quota_bytes === null ? '' : String(repo.quota_bytes / GIB),
    cleanup_policy_ids: repo.cleanup_policy_ids,
    routing_rule_id: repo.routing_rule_id ?? '',
    remote_url: repo.proxy?.remote_url ?? '',
    auth_username: repo.proxy?.auth?.username ?? '',
    content_max_age_secs: String(repo.proxy?.content_max_age_secs ?? 86400),
    metadata_max_age_secs: String(repo.proxy?.metadata_max_age_secs ?? 1800),
    negative_cache_ttl_secs: String(repo.proxy?.negative_cache_ttl_secs ?? 3600),
    max_concurrent_upstream: String(repo.proxy?.max_concurrent_upstream ?? 16),
    members: repo.group?.members ?? [],
    config: configValuesFrom(fields, repo.format_config),
  })
}

const quotaBytes = (gib: string): number | null =>
  gib.trim() === '' ? null : Math.round(Number(gib) * GIB)

export function toCreateRequest(
  v: RepositoryFormValues,
  fields: readonly ConfigField[],
): RepositoryCreate {
  const request: RepositoryCreate = {
    name: v.name.trim(),
    format: v.format as RepositoryCreate['format'],
    repo_type: v.repo_type,
    blob_store_id: v.blob_store_id,
    allow_anonymous: v.allow_anonymous,
    online: v.online,
    quota_bytes: quotaBytes(v.quota_gib),
    cleanup_policy_ids: v.cleanup_policy_ids,
    format_config: configToJson(fields, v.config),
  }
  if (v.repo_type === 'hosted') request.write_policy = v.write_policy
  if (v.repo_type !== 'hosted' && v.routing_rule_id !== '')
    request.routing_rule_id = v.routing_rule_id
  if (v.repo_type === 'proxy') {
    request.proxy = {
      remote_url: v.remote_url.trim(),
      auth:
        v.auth_username.trim() === ''
          ? null
          : {
              username: v.auth_username.trim(),
              ...(v.auth_password === '' ? {} : { password: v.auth_password }),
            },
      content_max_age_secs: Number(v.content_max_age_secs),
      metadata_max_age_secs: Number(v.metadata_max_age_secs),
      negative_cache_ttl_secs: Number(v.negative_cache_ttl_secs),
      max_concurrent_upstream: Number(v.max_concurrent_upstream),
    }
  }
  if (v.repo_type === 'group') request.group = { members: v.members }
  return request
}

const sameList = (a: readonly string[], b: readonly string[]): boolean =>
  a.length === b.length && a.every((value, index) => value === b[index])

/**
 * The merge patch of what the user changed (docs/api-conventions.md, 4): only changed fields
 * are sent, so an edit never overwrites a field it did not touch. `auth` follows the contract:
 * absent keeps the stored credentials, `null` removes them, and `auth` without `password` keeps
 * the stored password while the user name changes.
 */
export function toPatch(
  v: RepositoryFormValues,
  initial: RepositoryFormValues,
  fields: readonly ConfigField[],
): RepositoryPatch {
  const patch: RepositoryPatch = {}
  if (v.repo_type === 'hosted' && v.write_policy !== initial.write_policy) {
    patch.write_policy = v.write_policy
  }
  if (v.allow_anonymous !== initial.allow_anonymous) patch.allow_anonymous = v.allow_anonymous
  if (v.online !== initial.online) patch.online = v.online
  if (v.quota_gib !== initial.quota_gib) patch.quota_bytes = quotaBytes(v.quota_gib)
  if (!sameList(v.cleanup_policy_ids, initial.cleanup_policy_ids)) {
    patch.cleanup_policy_ids = v.cleanup_policy_ids
  }
  if (v.repo_type !== 'hosted' && v.routing_rule_id !== initial.routing_rule_id) {
    patch.routing_rule_id = v.routing_rule_id === '' ? null : v.routing_rule_id
  }
  if (v.repo_type === 'group' && !sameList(v.members, initial.members)) {
    patch.group = { members: v.members }
  }
  if (v.repo_type === 'proxy') {
    const proxy: NonNullable<RepositoryPatch['proxy']> = {}
    if (v.remote_url.trim() !== initial.remote_url) proxy.remote_url = v.remote_url.trim()
    if (v.content_max_age_secs !== initial.content_max_age_secs) {
      proxy.content_max_age_secs = Number(v.content_max_age_secs)
    }
    if (v.metadata_max_age_secs !== initial.metadata_max_age_secs) {
      proxy.metadata_max_age_secs = Number(v.metadata_max_age_secs)
    }
    if (v.negative_cache_ttl_secs !== initial.negative_cache_ttl_secs) {
      proxy.negative_cache_ttl_secs = Number(v.negative_cache_ttl_secs)
    }
    if (v.max_concurrent_upstream !== initial.max_concurrent_upstream) {
      proxy.max_concurrent_upstream = Number(v.max_concurrent_upstream)
    }
    if (v.remove_credentials) proxy.auth = null
    else if (v.auth_username !== initial.auth_username || v.auth_password !== '') {
      proxy.auth = {
        username: v.auth_username.trim(),
        ...(v.auth_password === '' ? {} : { password: v.auth_password }),
      }
    }
    if (Object.keys(proxy).length > 0) patch.proxy = proxy
  }
  const config = configToJson(fields, v.config)
  const before = configToJson(fields, initial.config)
  const changed = Object.fromEntries(
    Object.entries(config).filter(([key, value]) => before[key] !== value),
  )
  if (Object.keys(changed).length > 0) patch.format_config = changed
  return patch
}

/** What a failed request says about the form: which fields, and which wizard step holds the first. */
export type FormStep = 1 | 2

const STEP_ONE: ReadonlySet<keyof RepositoryFormValues> = new Set(['name', 'format', 'repo_type'])

const POINTERS: Readonly<Record<string, keyof RepositoryFormValues>> = {
  '/name': 'name',
  '/format': 'format',
  '/repo_type': 'repo_type',
  '/blob_store_id': 'blob_store_id',
  '/write_policy': 'write_policy',
  '/allow_anonymous': 'allow_anonymous',
  '/online': 'online',
  '/quota_bytes': 'quota_gib',
  '/cleanup_policy_ids': 'cleanup_policy_ids',
  '/routing_rule_id': 'routing_rule_id',
  '/proxy/remote_url': 'remote_url',
  '/proxy/auth/username': 'auth_username',
  '/proxy/auth/password': 'auth_password',
  '/proxy/content_max_age_secs': 'content_max_age_secs',
  '/proxy/metadata_max_age_secs': 'metadata_max_age_secs',
  '/proxy/negative_cache_ttl_secs': 'negative_cache_ttl_secs',
  '/proxy/max_concurrent_upstream': 'max_concurrent_upstream',
  '/group/members': 'members',
}

/** `/group/members/1` and `/cleanup_policy_ids/0` belong to the list field they sit in. */
function fieldOfPointer(pointer: string): keyof RepositoryFormValues | null {
  const exact = POINTERS[pointer]
  if (exact !== undefined) return exact
  const parent = pointer.replace(/\/\d+$/, '')
  const viaParent = POINTERS[parent]
  if (viaParent !== undefined) return viaParent
  return pointer.startsWith('/format_config') ? 'config' : null
}

export interface AppliedErrors {
  /** True when at least one message was put on a field. */
  applied: boolean
  /** Lowest wizard step with a field error: the wizard goes back to it. */
  step: FormStep | null
}

/**
 * Put the failure of a create or save on the fields it names (`details.fields`, JSON pointers
 * of the request body) and on the fields the code implies: `already_exists` is the name,
 * `upstream_url_rejected` the remote URL with the SSRF rule that refused it. The first
 * message gets the focus.
 */
export function applyServerErrors(
  form: UseFormReturn<RepositoryFormValues>,
  error: unknown,
): AppliedErrors {
  if (!isApiError(error)) return { applied: false, step: null }
  const messages: { name: keyof RepositoryFormValues; message: string }[] = []
  for (const f of error.fieldErrors) {
    const name = fieldOfPointer(f.field)
    if (name !== null) messages.push({ name, message: f.message })
  }
  if (error.code === 'already_exists') {
    messages.push({ name: 'name', message: t('repoForm.errors.nameTaken') })
  }
  if (error.code === 'upstream_url_rejected') {
    const rule = error.details?.rule
    messages.push({
      name: 'remote_url',
      message:
        typeof rule === 'string'
          ? t('repoForm.errors.urlRejectedRule', { rule })
          : t('repoForm.errors.urlRejected'),
    })
  }
  messages.forEach(({ name, message }, index) => {
    form.setError(name, { type: 'server', message }, { shouldFocus: index === 0 })
  })
  const step: FormStep | null =
    messages.length === 0 ? null : messages.some((m) => STEP_ONE.has(m.name)) ? 1 : 2
  return { applied: messages.length > 0, step }
}
