import '@/lib/zod-setup'
import { z } from 'zod'
import { t } from '@/i18n'
import type { BlobStoreCreate, BlobStorePatch, BlobStoreView } from './api'

/**
 * The blob store form keeps every value as text or a boolean (what the inputs hold) and turns
 * it into the API body in pure functions below, so validation, mapping and the PATCH diff are
 * testable without rendering anything. No value in this module is ever read back from the
 * server: credentials are write only, so the form can never be filled with them.
 */

const GIB = 1024 ** 3
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]*$/
const REF_PATTERN = /^(env:[A-Za-z_][A-Za-z0-9_]*|file:\/.+)$/

export const CREDENTIAL_SOURCES = ['default', 'inline', 'reference'] as const
export type CredentialSource = (typeof CREDENTIAL_SOURCES)[number]

export interface BlobStoreFormValues {
  name: string
  kind: 'local' | 's3'
  path: string
  endpoint: string
  bucket: string
  region: string
  keyPrefix: string
  pathStyle: boolean
  partSizeMb: string
  /** `default` in the create form means "no credentials in the body"; the edit form calls it "keep". */
  credentialSource: CredentialSource
  accessKey: string
  secretKey: string
  accessKeyRef: string
  secretKeyRef: string
  quotaGiB: string
}

export const EMPTY_VALUES: BlobStoreFormValues = {
  name: '',
  kind: 'local',
  path: '',
  endpoint: '',
  bucket: '',
  region: '',
  keyPrefix: '',
  pathStyle: true,
  partSizeMb: '16',
  credentialSource: 'default',
  accessKey: '',
  secretKey: '',
  accessKeyRef: '',
  secretKeyRef: '',
  quotaGiB: '',
}

function parseQuota(text: string): number | null {
  const trimmed = text.trim()
  if (trimmed === '') return null
  const value = Number(trimmed)
  return Number.isFinite(value) && value > 0 ? value : Number.NaN
}

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:'
  } catch {
    return false
  }
}

function issue(ctx: z.RefinementCtx, path: keyof BlobStoreFormValues, message: string): void {
  ctx.addIssue({ code: 'custom', path: [path], message })
}

function checkCredentials(v: BlobStoreFormValues, ctx: z.RefinementCtx): void {
  if (v.credentialSource === 'inline') {
    if (v.accessKey.trim() === '') issue(ctx, 'accessKey', t('blobStores.errors.accessKeyRequired'))
    if (v.secretKey === '') issue(ctx, 'secretKey', t('blobStores.errors.secretKeyRequired'))
  }
  if (v.credentialSource === 'reference') {
    for (const field of ['accessKeyRef', 'secretKeyRef'] as const) {
      const value = v[field].trim()
      if (value === '') issue(ctx, field, t('blobStores.errors.refRequired'))
      else if (!REF_PATTERN.test(value)) issue(ctx, field, t('blobStores.errors.refFormat'))
    }
  }
}

function checkConfig(v: BlobStoreFormValues, ctx: z.RefinementCtx, locked: boolean): void {
  if (v.kind === 'local') {
    if (locked) return
    if (v.path.trim() === '') issue(ctx, 'path', t('blobStores.errors.pathRequired'))
    else if (!v.path.trim().startsWith('/')) issue(ctx, 'path', t('blobStores.errors.pathAbsolute'))
    return
  }
  if (!locked) {
    if (v.endpoint.trim() === '') issue(ctx, 'endpoint', t('blobStores.errors.endpointRequired'))
    else if (!isHttpUrl(v.endpoint.trim())) {
      issue(ctx, 'endpoint', t('blobStores.errors.endpointFormat'))
    }
    if (v.bucket.trim() === '') issue(ctx, 'bucket', t('blobStores.errors.bucketRequired'))
  }
  if (v.region.trim() === '') issue(ctx, 'region', t('blobStores.errors.regionRequired'))
  const part = Number(v.partSizeMb)
  if (!Number.isInteger(part) || part < 5 || part > 1024) {
    issue(ctx, 'partSizeMb', t('blobStores.errors.partSize'))
  }
}

const textField = z.string()

const baseShape = {
  name: textField,
  kind: z.enum(['local', 's3']),
  path: textField,
  endpoint: textField,
  bucket: textField,
  region: textField,
  keyPrefix: textField,
  pathStyle: z.boolean(),
  partSizeMb: textField,
  credentialSource: z.enum(CREDENTIAL_SOURCES),
  accessKey: textField,
  secretKey: textField,
  accessKeyRef: textField,
  secretKeyRef: textField,
  quotaGiB: textField,
}

function checkQuota(v: BlobStoreFormValues, ctx: z.RefinementCtx): void {
  if (Number.isNaN(parseQuota(v.quotaGiB))) issue(ctx, 'quotaGiB', t('blobStores.errors.quota'))
}

export const createSchema = z.object(baseShape).superRefine((v, ctx) => {
  const name = v.name.trim()
  if (name === '') issue(ctx, 'name', t('blobStores.errors.nameRequired'))
  else if (name.length > 63) issue(ctx, 'name', t('blobStores.errors.nameLength'))
  else if (!NAME_PATTERN.test(name)) issue(ctx, 'name', t('blobStores.errors.nameFormat'))
  checkConfig(v, ctx, false)
  if (v.kind === 's3') checkCredentials(v, ctx)
  checkQuota(v, ctx)
})

/** `locked`: repositories use the store, so path, bucket and endpoint cannot change (contract). */
export function editSchema(locked: boolean) {
  return z.object(baseShape).superRefine((v, ctx) => {
    checkConfig(v, ctx, locked)
    if (v.kind === 's3') checkCredentials(v, ctx)
    checkQuota(v, ctx)
  })
}

/** The edit form starts from the stored view: never from credentials, which are not in it. */
export function valuesFromView(view: BlobStoreView): BlobStoreFormValues {
  const config = view.config
  const str = (key: string, fallback = ''): string => {
    const value = config[key]
    return typeof value === 'string' ? value : fallback
  }
  const part = config.part_size_mb
  return {
    ...EMPTY_VALUES,
    name: view.name,
    kind: view.kind,
    path: str('path'),
    endpoint: str('endpoint'),
    bucket: str('bucket'),
    region: str('region'),
    keyPrefix: str('key_prefix'),
    pathStyle: config.path_style !== false,
    partSizeMb: typeof part === 'number' ? String(part) : '16',
    quotaGiB:
      view.quota_bytes === null ? '' : String(Math.round((view.quota_bytes / GIB) * 100) / 100),
  }
}

function credentialsOf(v: BlobStoreFormValues) {
  if (v.credentialSource === 'inline') {
    return { access_key: v.accessKey.trim(), secret_key: v.secretKey }
  }
  if (v.credentialSource === 'reference') {
    return { access_key_ref: v.accessKeyRef.trim(), secret_key_ref: v.secretKeyRef.trim() }
  }
  return undefined
}

function quotaBytes(v: BlobStoreFormValues): number | null {
  const quota = parseQuota(v.quotaGiB)
  return quota === null || Number.isNaN(quota) ? null : Math.round(quota * GIB)
}

export function toCreateBody(v: BlobStoreFormValues): BlobStoreCreate {
  const name = v.name.trim()
  if (v.kind === 'local') {
    return { name, kind: 'local', config: { path: v.path.trim() }, quota_bytes: quotaBytes(v) }
  }
  return {
    name,
    kind: 's3',
    config: {
      endpoint: v.endpoint.trim(),
      bucket: v.bucket.trim(),
      region: v.region.trim(),
      key_prefix: v.keyPrefix.trim(),
      path_style: v.pathStyle,
      part_size_mb: Number(v.partSizeMb),
    },
    credentials: credentialsOf(v),
    quota_bytes: quotaBytes(v),
  }
}

/**
 * A merge patch of only what the user changed (RFC 7396): `config` merges key by key,
 * `credentials` is sent only when the user entered a new pair, `quota_bytes: null` clears the
 * quota. An empty object means nothing changed.
 */
export function buildPatch(view: BlobStoreView, v: BlobStoreFormValues): BlobStorePatch {
  const initial = valuesFromView(view)
  type Scalar = string | number | boolean
  const candidates: readonly (readonly [string, Scalar, Scalar])[] =
    view.kind === 'local'
      ? [['path', v.path.trim(), initial.path]]
      : [
          ['endpoint', v.endpoint.trim(), initial.endpoint],
          ['bucket', v.bucket.trim(), initial.bucket],
          ['region', v.region.trim(), initial.region],
          ['key_prefix', v.keyPrefix.trim(), initial.keyPrefix],
          ['path_style', v.pathStyle, initial.pathStyle],
          ['part_size_mb', Number(v.partSizeMb), Number(initial.partSizeMb)],
        ]
  const changed = candidates.filter(([, now, before]) => now !== before)
  const credentials = view.kind === 's3' ? credentialsOf(v) : undefined
  const quota = quotaBytes(v)
  return {
    ...(changed.length > 0
      ? { config: Object.fromEntries(changed.map(([key, now]) => [key, now])) }
      : {}),
    ...(credentials !== undefined ? { credentials } : {}),
    ...(quota !== view.quota_bytes ? { quota_bytes: quota } : {}),
  }
}

/**
 * What "Copy my changes" puts on the clipboard after a conflict: the patch WITHOUT credential
 * values. A secret must not travel through the clipboard by accident.
 */
export function redactPatch(patch: BlobStorePatch): string {
  const { credentials, ...rest } = patch
  return JSON.stringify(
    credentials === undefined ? rest : { ...rest, credentials: '[not copied]' },
    null,
    2,
  )
}

export type QuotaState = 'none' | 'ok' | 'near' | 'over'

/** Share of the soft quota from which the status says "near". */
export const NEAR_QUOTA_RATIO = 0.8

export interface UsageDisplay {
  /** 0 to 100, clamped, for the bar; null when there is nothing to measure against. */
  percent: number | null
  limitBytes: number | null
  basis: 'quota' | 'disk' | 'none'
  quota: QuotaState
}

/** The bar measures against the soft quota when there is one, else against the disk. */
export function usageDisplay(view: Pick<BlobStoreView, 'usage' | 'quota_bytes'>): UsageDisplay {
  const used = view.usage?.used_bytes ?? null
  if (used === null) return { percent: null, limitBytes: null, basis: 'none', quota: 'none' }
  if (view.quota_bytes !== null && view.quota_bytes > 0) {
    const ratio = used / view.quota_bytes
    return {
      percent: Math.min(100, Math.round(ratio * 100)),
      limitBytes: view.quota_bytes,
      basis: 'quota',
      quota: ratio >= 1 ? 'over' : ratio >= NEAR_QUOTA_RATIO ? 'near' : 'ok',
    }
  }
  const total = view.usage?.total_bytes ?? null
  if (total !== null && total > 0) {
    return {
      percent: Math.min(100, Math.round((used / total) * 100)),
      limitBytes: total,
      basis: 'disk',
      quota: 'none',
    }
  }
  return { percent: null, limitBytes: null, basis: 'none', quota: 'none' }
}
