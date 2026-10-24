import { describe, expect, it } from 'vitest'
import {
  describeCriteria,
  describeFormCriteria,
  EMPTY_POLICY,
  policySchema,
  toInput,
  valuesFromPolicy,
  type CleanupPolicyView,
} from './criteria'

const none = {
  older_than_days: null,
  last_downloaded_days: null,
  keep_last_versions: null,
  name_pattern: null,
  release_type: null,
}

describe('criteria', () => {
  it('describes every criterion in words', () => {
    expect(
      describeCriteria({
        older_than_days: 90,
        last_downloaded_days: 30,
        keep_last_versions: 5,
        name_pattern: '^ci-',
        release_type: 'release',
      }),
    ).toEqual([
      'older than 90 days',
      'not downloaded for 30 days',
      'older than the newest 5 versions of its package',
      'name matches ^ci-',
      'release versions only',
    ])
    expect(describeCriteria(none)).toEqual([])
  })

  it('describes the form while typing and ignores half typed numbers', () => {
    expect(
      describeFormCriteria({ ...EMPTY_POLICY, older_than_days: '12', keep_last_versions: 'x' }),
    ).toEqual(['older than 12 days'])
    expect(
      describeFormCriteria({ ...EMPTY_POLICY, release_type: 'prerelease', name_pattern: ' a ' }),
    ).toEqual(['name matches a', 'pre-release versions only'])
  })

  it('round trips a policy through the form', () => {
    const policy = {
      id: 'p',
      name: 'n',
      description: 'd',
      format: 'maven2',
      criteria: { ...none, older_than_days: 7, release_type: 'prerelease' },
      created_at: 'x',
      updated_at: 'x',
      attached_repositories: [],
      last_run: null,
    } as CleanupPolicyView
    const values = valuesFromPolicy(policy)
    expect(values).toMatchObject({
      older_than_days: '7',
      release_type: 'prerelease',
      name_pattern: '',
    })
    expect(toInput(values)).toEqual({
      name: 'n',
      description: 'd',
      format: 'maven2',
      criteria: { ...none, older_than_days: 7, release_type: 'prerelease' },
    })
  })

  it('needs a name, whole numbers and at least one criterion', () => {
    expect(policySchema.safeParse(EMPTY_POLICY).success).toBe(false)
    expect(
      policySchema.safeParse({ ...EMPTY_POLICY, name: 'x'.repeat(129), older_than_days: '1' })
        .success,
    ).toBe(false)
    expect(
      policySchema.safeParse({ ...EMPTY_POLICY, name: 'x', older_than_days: '1.5' }).success,
    ).toBe(false)
    expect(
      policySchema.safeParse({ ...EMPTY_POLICY, name: 'x', name_pattern: 'a'.repeat(513) }).success,
    ).toBe(false)
    expect(policySchema.safeParse({ ...EMPTY_POLICY, name: 'x', name_pattern: '^a' }).success).toBe(
      true,
    )
    expect(
      policySchema.safeParse({ ...EMPTY_POLICY, name: 'x', release_type: 'release' }).success,
    ).toBe(true)
  })
})
