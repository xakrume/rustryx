import '@/lib/zod-setup'
import { z } from 'zod'
import { t } from '@/i18n'
import type { components } from '@/api/schema'

export type CleanupCriteria = components['schemas']['CleanupCriteria']
export type CleanupPolicyView = components['schemas']['CleanupPolicyView']
export type CleanupPolicyInput = components['schemas']['CleanupPolicyInput']
export type RepoFormat = components['schemas']['RepoFormat']

export const CRITERIA = [
  'older_than_days',
  'last_downloaded_days',
  'keep_last_versions',
  'name_pattern',
  'release_type',
] as const
export type CriterionId = (typeof CRITERIA)[number]

export const NUMERIC_CRITERIA = [
  'older_than_days',
  'last_downloaded_days',
  'keep_last_versions',
] as const

/**
 * The editor keeps every criterion as the text of its input: empty means "not set". Numbers
 * become numbers only in `toInput`, so a half typed value is an error message, not a NaN.
 */
export interface PolicyFormValues {
  name: string
  description: string
  format: RepoFormat
  older_than_days: string
  last_downloaded_days: string
  keep_last_versions: string
  name_pattern: string
  release_type: '' | 'release' | 'prerelease'
}

export const EMPTY_POLICY: PolicyFormValues = {
  name: '',
  description: '',
  format: 'maven2',
  older_than_days: '',
  last_downloaded_days: '',
  keep_last_versions: '',
  name_pattern: '',
  release_type: '',
}

const whole = (text: string): boolean => /^\d+$/.test(text.trim()) && Number(text) >= 1

const shape = {
  name: z.string(),
  description: z.string(),
  format: z.enum(['maven2', 'npm', 'pypi', 'docker', 'oci', 'helm', 'raw', 'pub', 'rpm', 'apt', 'apk']),
  older_than_days: z.string(),
  last_downloaded_days: z.string(),
  keep_last_versions: z.string(),
  name_pattern: z.string(),
  release_type: z.enum(['', 'release', 'prerelease']),
}

/** `at least one criterion` is a form level error: `criteria` is its path, shown above Save. */
export const policySchema = z.object(shape).superRefine((v, ctx) => {
  const name = v.name.trim()
  if (name === '')
    ctx.addIssue({ code: 'custom', path: ['name'], message: t('cleanup.errors.nameRequired') })
  else if (name.length > 128) {
    ctx.addIssue({ code: 'custom', path: ['name'], message: t('cleanup.errors.nameLength') })
  }
  for (const key of NUMERIC_CRITERIA) {
    if (v[key].trim() !== '' && !whole(v[key])) {
      ctx.addIssue({ code: 'custom', path: [key], message: t('cleanup.errors.number') })
    }
  }
  if (v.name_pattern.length > 512) {
    ctx.addIssue({
      code: 'custom',
      path: ['name_pattern'],
      message: t('cleanup.errors.patternLength'),
    })
  }
  const anySet =
    NUMERIC_CRITERIA.some((key) => v[key].trim() !== '') ||
    v.name_pattern.trim() !== '' ||
    v.release_type !== ''
  if (!anySet) {
    ctx.addIssue({ code: 'custom', path: ['criteria'], message: t('cleanup.criteria.atLeastOne') })
  }
})

export function valuesFromPolicy(policy: CleanupPolicyView): PolicyFormValues {
  const c = policy.criteria
  return {
    name: policy.name,
    description: policy.description,
    format: policy.format,
    older_than_days: c.older_than_days === null ? '' : String(c.older_than_days),
    last_downloaded_days: c.last_downloaded_days === null ? '' : String(c.last_downloaded_days),
    keep_last_versions: c.keep_last_versions === null ? '' : String(c.keep_last_versions),
    name_pattern: c.name_pattern ?? '',
    release_type: c.release_type ?? '',
  }
}

const numberOrNull = (text: string): number | null => (text.trim() === '' ? null : Number(text))

export function toInput(v: PolicyFormValues): CleanupPolicyInput {
  return {
    name: v.name.trim(),
    description: v.description.trim(),
    format: v.format,
    criteria: {
      older_than_days: numberOrNull(v.older_than_days),
      last_downloaded_days: numberOrNull(v.last_downloaded_days),
      keep_last_versions: numberOrNull(v.keep_last_versions),
      name_pattern: v.name_pattern.trim() === '' ? null : v.name_pattern.trim(),
      release_type: v.release_type === '' ? null : v.release_type,
    },
  }
}

/** The criteria that are set, in plain words ("older than 90 days"), for the list and the editor. */
export function describeCriteria(criteria: CleanupCriteria): string[] {
  const parts: string[] = []
  if (criteria.older_than_days !== null) {
    parts.push(t('cleanup.criteria.older_than_days.summary', { value: criteria.older_than_days }))
  }
  if (criteria.last_downloaded_days !== null) {
    parts.push(
      t('cleanup.criteria.last_downloaded_days.summary', { value: criteria.last_downloaded_days }),
    )
  }
  if (criteria.keep_last_versions !== null) {
    parts.push(
      t('cleanup.criteria.keep_last_versions.summary', { value: criteria.keep_last_versions }),
    )
  }
  if (criteria.name_pattern !== null) {
    parts.push(t('cleanup.criteria.name_pattern.summary', { value: criteria.name_pattern }))
  }
  if (criteria.release_type !== null) {
    parts.push(
      t('cleanup.criteria.release_type.summary', {
        value: t(`cleanup.criteria.release_type.${criteria.release_type}`),
      }),
    )
  }
  return parts
}

/** The form's criteria as the same words, so the sentence updates while the user types. */
export function describeFormCriteria(v: PolicyFormValues): string[] {
  const valid = (text: string): number | null => (whole(text) ? Number(text) : null)
  return describeCriteria({
    older_than_days: valid(v.older_than_days),
    last_downloaded_days: valid(v.last_downloaded_days),
    keep_last_versions: valid(v.keep_last_versions),
    name_pattern: v.name_pattern.trim() === '' ? null : v.name_pattern.trim(),
    release_type: v.release_type === '' ? null : v.release_type,
  })
}
