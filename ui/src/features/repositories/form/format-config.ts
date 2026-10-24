/**
 * The repository form for the options of ONE format is generated from the JSON Schema
 * (`config_schema`) that `GET /api/v1/formats` serves, so format knowledge stays in the format
 * crate (docs/api-conventions.md, FormatInfo). Only the small subset the format crates use is
 * understood: a flat object of enum, boolean, integer, number and string properties. Anything
 * else (a nested object, an array) is left out of the form, and the server keeps its value
 * untouched because a merge patch only sends the keys the form owns.
 */
export type ConfigFieldKind = 'enum' | 'boolean' | 'integer' | 'string'

export interface ConfigField {
  key: string
  label: string
  description: string | null
  kind: ConfigFieldKind
  options: readonly string[]
  /** The schema default, already in form representation (booleans stay booleans). */
  initial: string | boolean
}

export type ConfigValue = string | boolean
export type ConfigValues = Readonly<Record<string, ConfigValue>>

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** Form text for a schema or stored primitive. Objects are not form values. */
function primitiveText(value: unknown): string | null {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
    return String(value)
  }
  return null
}

/** "version_policy" becomes "Version policy" when the schema has no title. */
export function humanize(key: string): string {
  const spaced = key.replace(/[_-]+/g, ' ').trim()
  return spaced.charAt(0).toUpperCase() + spaced.slice(1)
}

function kindOf(property: Record<string, unknown>): ConfigFieldKind | null {
  if (Array.isArray(property.enum) && property.enum.every((v) => typeof v === 'string')) {
    return 'enum'
  }
  switch (property.type) {
    case 'boolean':
      return 'boolean'
    case 'integer':
    case 'number':
      return 'integer'
    case 'string':
      return 'string'
    default:
      return null
  }
}

export function configFields(schema: unknown): ConfigField[] {
  if (!isRecord(schema) || !isRecord(schema.properties)) return []
  const fields: ConfigField[] = []
  for (const [key, raw] of Object.entries(schema.properties)) {
    if (!isRecord(raw)) continue
    const kind = kindOf(raw)
    if (kind === null) continue
    const options = kind === 'enum' ? (raw.enum as string[]) : []
    const fallback = kind === 'boolean' ? false : kind === 'enum' ? (options[0] ?? '') : ''
    const initial =
      kind === 'boolean'
        ? raw.default === true
        : raw.default === undefined || raw.default === null
          ? fallback
          : (primitiveText(raw.default) ?? fallback)
    fields.push({
      key,
      label: typeof raw.title === 'string' ? raw.title : humanize(key),
      description: typeof raw.description === 'string' ? raw.description : null,
      kind,
      options,
      initial,
    })
  }
  return fields
}

/** The form's initial values: stored values win over schema defaults. */
export function configValuesFrom(
  fields: readonly ConfigField[],
  stored: Readonly<Record<string, unknown>>,
): Record<string, ConfigValue> {
  return Object.fromEntries(
    fields.map((field) => {
      const value = stored[field.key]
      if (value === undefined || value === null) return [field.key, field.initial]
      if (field.kind === 'boolean') return [field.key, value === true]
      return [field.key, primitiveText(value) ?? field.initial]
    }),
  )
}

/** Form representation back to the JSON the API stores: integers become numbers, empty text is left out. */
export function configToJson(
  fields: readonly ConfigField[],
  values: ConfigValues,
): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const field of fields) {
    const value = values[field.key]
    if (value === undefined) continue
    if (field.kind === 'boolean') out[field.key] = value === true
    else if (field.kind === 'integer') {
      if (typeof value === 'string' && value.trim() !== '' && Number.isFinite(Number(value))) {
        out[field.key] = Number(value)
      }
    } else if (typeof value === 'string' && value !== '') out[field.key] = value
  }
  return out
}
