import { describe, expect, it } from 'vitest'
import { buildSchema, changedValues, fieldsOf, valuesFromSection } from './schema-model'

const schema = {
  type: 'object',
  properties: {
    ttl_days: { type: 'integer', title: 'Lifetime', minimum: 1, maximum: 365 },
    ratio: { type: 'number', minimum: 0.5 },
    cap: { type: 'integer', maximum: 10 },
    on: { type: 'boolean' },
    mode: { type: 'string', enum: ['a', 'b'] },
    label: { type: 'string', description: 'Free text' },
  },
}

describe('fieldsOf', () => {
  it('reads titles, bounds, enums and falls back to a readable name', () => {
    const fields = fieldsOf(schema)
    expect(fields.find((f) => f.key === 'ttl_days')).toMatchObject({
      title: 'Lifetime',
      minimum: 1,
      maximum: 365,
    })
    expect(fields.find((f) => f.key === 'ratio')?.title).toBe('Ratio')
    expect(fields.find((f) => f.key === 'mode')?.enumValues).toEqual(['a', 'b'])
    expect(fields.find((f) => f.key === 'label')?.description).toBe('Free text')
    expect(fieldsOf(null)).toEqual([])
    expect(fieldsOf({ properties: [] })).toEqual([])
  })
})

describe('section form model', () => {
  const fields = fieldsOf(schema)
  const section = {
    values: { ttl_days: 90, ratio: 1.5, cap: null, on: true, mode: 'a', label: 'x' },
  }
  const editable = ['ttl_days', 'ratio', 'cap', 'on', 'mode', 'label']

  it('fills the form with text, and booleans as booleans', () => {
    expect(valuesFromSection(fields, section)).toEqual({
      ttl_days: '90',
      ratio: '1.5',
      cap: '',
      on: true,
      mode: 'a',
      label: 'x',
    })
  })

  it('checks only editable fields, with the right message for each bound', () => {
    const zod = buildSchema(fields, ['ttl_days', 'ratio', 'cap', 'label'])
    const base = { ttl_days: '90', ratio: '1', cap: '5', on: true, mode: 'a', label: 'x' }
    expect(zod.safeParse(base).success).toBe(true)
    const messages = (values: Record<string, unknown>) =>
      zod.safeParse(values).error?.issues.map((i) => i.message) ?? []
    expect(messages({ ...base, ttl_days: '' })).toEqual(['Enter a value.'])
    expect(messages({ ...base, ttl_days: '1.5' })).toEqual(['Enter a whole number.'])
    expect(messages({ ...base, ttl_days: '999' })).toEqual([
      'Enter a whole number between 1 and 365.',
    ])
    expect(messages({ ...base, ratio: '0.1' })).toEqual(['Enter a whole number of 0.5 or more.'])
    expect(messages({ ...base, cap: '11' })).toEqual(['Enter a whole number of 10 or less.'])
    expect(messages({ ...base, label: ' ' })).toEqual(['Enter a value.'])
    // `mode` is not editable here, so an empty value is not checked.
    expect(zod.safeParse({ ...base, mode: '' }).success).toBe(true)
  })

  it('sends only what changed, numbers as numbers', () => {
    const initial = valuesFromSection(fields, section)
    expect(changedValues(fields, editable, initial, initial)).toEqual({})
    expect(
      changedValues(fields, editable, initial, {
        ...initial,
        ttl_days: '60',
        on: false,
        label: 'y',
      }),
    ).toEqual({
      ttl_days: 60,
      on: false,
      label: 'y',
    })
    expect(changedValues(fields, ['on'], initial, { ...initial, ttl_days: '1' })).toEqual({})
  })
})
