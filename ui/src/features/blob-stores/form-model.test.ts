import { describe, expect, it } from 'vitest'
import type { BlobStoreView } from './api'
import {
  buildPatch,
  createSchema,
  editSchema,
  EMPTY_VALUES,
  redactPatch,
  toCreateBody,
  usageDisplay,
  valuesFromView,
} from './form-model'

const GIB = 1024 ** 3

const view = (over: Partial<BlobStoreView> = {}): BlobStoreView => ({
  id: 'id',
  name: 'archive',
  kind: 's3',
  config: {
    endpoint: 'https://seaweedfs.example.org',
    bucket: 'b',
    region: 'us-east-1',
    key_prefix: '',
    path_style: true,
    part_size_mb: 16,
  },
  quota_bytes: null,
  created_at: '2026-10-24T00:00:00Z',
  usage: {
    used_bytes: 10,
    blob_count: 1,
    available_bytes: 90,
    total_bytes: 100,
    as_of: '2026-10-24T00:00:00Z',
  },
  health: { state: 'ok', checked_at: null, last_error_code: null },
  repository_count: 0,
  credentials: null,
  ...over,
})

describe('usageDisplay', () => {
  it('measures against the soft quota and names the state', () => {
    expect(usageDisplay(view({ quota_bytes: 100 }))).toMatchObject({
      basis: 'quota',
      quota: 'ok',
      percent: 10,
    })
    expect(usageDisplay(view({ quota_bytes: 12 }))).toMatchObject({ quota: 'near' })
    expect(usageDisplay(view({ quota_bytes: 10 }))).toMatchObject({ quota: 'over', percent: 100 })
  })

  it('falls back to the disk, then to nothing', () => {
    expect(usageDisplay(view())).toMatchObject({ basis: 'disk', percent: 10 })
    expect(
      usageDisplay(
        view({
          usage: {
            used_bytes: 5,
            blob_count: 0,
            available_bytes: null,
            total_bytes: null,
            as_of: 'x',
          },
        }),
      ),
    ).toMatchObject({ basis: 'none', percent: null })
    expect(usageDisplay(view({ usage: null }))).toMatchObject({ basis: 'none' })
  })
})

describe('create form', () => {
  it('validates the name, the paths and the references', () => {
    const bad = createSchema.safeParse({ ...EMPTY_VALUES, name: '-x', path: 'relative' })
    expect(bad.success).toBe(false)
    const long = createSchema.safeParse({ ...EMPTY_VALUES, name: 'a'.repeat(64), path: '/p' })
    expect(long.success).toBe(false)
    const s3 = createSchema.safeParse({
      ...EMPTY_VALUES,
      name: 'ok',
      kind: 's3',
      endpoint: 'ftp://x',
      bucket: '',
      region: '',
      partSizeMb: '2',
      credentialSource: 'reference',
      accessKeyRef: 'nope',
      secretKeyRef: '',
      quotaGiB: '-1',
    })
    expect(s3.success).toBe(false)
    const good = createSchema.safeParse({ ...EMPTY_VALUES, name: 'ok', path: '/data' })
    expect(good.success).toBe(true)
  })

  it('builds the create bodies for both kinds, with credentials only when entered', () => {
    expect(toCreateBody({ ...EMPTY_VALUES, name: ' x ', path: ' /data ', quotaGiB: '2' })).toEqual({
      name: 'x',
      kind: 'local',
      config: { path: '/data' },
      quota_bytes: 2 * GIB,
    })
    const s3 = toCreateBody({
      ...EMPTY_VALUES,
      name: 's',
      kind: 's3',
      endpoint: 'https://e',
      bucket: 'b',
      region: 'r',
      credentialSource: 'inline',
      accessKey: 'AK',
      secretKey: 'SK',
    })
    expect(s3).toMatchObject({
      kind: 's3',
      credentials: { access_key: 'AK', secret_key: 'SK' },
      quota_bytes: null,
    })
  })
})

describe('edit form', () => {
  it('fills the form from the view and sends only what changed', () => {
    const v = view({ quota_bytes: 100 * GIB })
    const values = valuesFromView(v)
    expect(values.quotaGiB).toBe('100')
    expect(buildPatch(v, values)).toEqual({})
    expect(buildPatch(v, { ...values, region: 'eu-west-1', quotaGiB: '' })).toEqual({
      config: { region: 'eu-west-1' },
      quota_bytes: null,
    })
    expect(
      buildPatch(v, {
        ...values,
        credentialSource: 'reference',
        accessKeyRef: 'env:A',
        secretKeyRef: 'env:B',
      }),
    ).toEqual({ credentials: { access_key_ref: 'env:A', secret_key_ref: 'env:B' } })
    const local = view({ kind: 'local', config: { path: '/a' } })
    expect(buildPatch(local, { ...valuesFromView(local), path: '/b' })).toEqual({
      config: { path: '/b' },
    })
  })

  it('does not check the locked settings but still checks the region', () => {
    const v = valuesFromView(view())
    expect(editSchema(true).safeParse({ ...v, endpoint: '', bucket: '' }).success).toBe(true)
    expect(editSchema(false).safeParse({ ...v, endpoint: '', bucket: '' }).success).toBe(false)
    expect(editSchema(true).safeParse({ ...v, region: '' }).success).toBe(false)
  })

  it('redacts credentials when a patch is copied', () => {
    expect(redactPatch({ quota_bytes: 1, credentials: { secret_key: 'SK' } })).not.toContain('SK')
    expect(redactPatch({ quota_bytes: 1 })).not.toContain('credentials')
  })
})
