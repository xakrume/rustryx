import '@/lib/zod-setup'
import { z } from 'zod'
import { scalarText } from '@/features/system/scalar-text'
import { t } from '@/i18n'
import type { SettingsSection } from './api'

/** What the form needs of a JSON Schema property (draft 2020-12, the part the sections use). */
export interface FieldSpec {
  key: string
  type: 'integer' | 'number' | 'boolean' | 'string'
  title: string
  description: string | null
  minimum: number | null
  maximum: number | null
  enumValues: readonly string[] | null
}

type Json = Record<string, unknown>
const asRecord = (value: unknown): Json =>
  typeof value === 'object' && value !== null && !Array.isArray(value) ? (value as Json) : {}

/** `session_idle_timeout_secs` as a fallback title: "Session idle timeout secs". */
function humanize(key: string): string {
  const text = key.replace(/_/g, ' ')
  return text.charAt(0).toUpperCase() + text.slice(1)
}

export function fieldsOf(schema: unknown): FieldSpec[] {
  const properties = asRecord(asRecord(schema).properties)
  return Object.entries(properties).map(([key, raw]) => {
    const spec = asRecord(raw)
    const type =
      spec.type === 'integer' || spec.type === 'number' || spec.type === 'boolean'
        ? spec.type
        : 'string'
    return {
      key,
      type,
      title: typeof spec.title === 'string' ? spec.title : humanize(key),
      description: typeof spec.description === 'string' ? spec.description : null,
      minimum: typeof spec.minimum === 'number' ? spec.minimum : null,
      maximum: typeof spec.maximum === 'number' ? spec.maximum : null,
      enumValues: Array.isArray(spec.enum) ? spec.enum.map(String) : null,
    }
  })
}

/** Form values by key: text for everything but booleans, so a half typed number is a message, not NaN. */
export type SectionFormValues = Record<string, string | boolean>

export function valuesFromSection(
  fields: readonly FieldSpec[],
  section: Pick<SettingsSection, 'values'>,
): SectionFormValues {
  return Object.fromEntries(
    fields.map((f) => {
      const value = section.values[f.key]
      return [f.key, f.type === 'boolean' ? value === true : scalarText(value)]
    }),
  )
}

function checkNumber(spec: FieldSpec, text: string): string | null {
  if (text.trim() === '') return t('systemSettings.requiredValue')
  const value = Number(text)
  if (!Number.isFinite(value) || (spec.type === 'integer' && !Number.isInteger(value))) {
    return t('systemSettings.whole')
  }
  const { minimum: min, maximum: max } = spec
  if (min !== null && max !== null && (value < min || value > max)) {
    return t('systemSettings.range', { min, max })
  }
  if (min !== null && value < min) return t('systemSettings.rangeMin', { min })
  if (max !== null && value > max) return t('systemSettings.rangeMax', { max })
  return null
}

/** Only editable fields are checked: the others are shown, never sent. */
export function buildSchema(fields: readonly FieldSpec[], editable: readonly string[]) {
  const shape = Object.fromEntries(
    fields.map((f) => [f.key, f.type === 'boolean' ? z.boolean() : z.string()]),
  )
  return z.object(shape).superRefine((values, ctx) => {
    for (const field of fields) {
      if (!editable.includes(field.key) || field.type === 'boolean') continue
      const text = values[field.key]
      if (typeof text !== 'string') continue
      const message =
        field.type === 'string'
          ? text.trim() === '' && field.enumValues === null
            ? t('systemSettings.requiredValue')
            : null
          : checkNumber(field, text)
      if (message !== null) ctx.addIssue({ code: 'custom', path: [field.key], message })
    }
  })
}

/** The fields the user changed, with numbers as numbers: the body of the update. */
export function changedValues(
  fields: readonly FieldSpec[],
  editable: readonly string[],
  initial: SectionFormValues,
  now: SectionFormValues,
): Record<string, unknown> {
  return Object.fromEntries(
    fields
      .filter((f) => editable.includes(f.key) && initial[f.key] !== now[f.key])
      .map((f) => {
        const value = now[f.key]
        return [f.key, f.type === 'integer' || f.type === 'number' ? Number(value) : value]
      }),
  )
}
