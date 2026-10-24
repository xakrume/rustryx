import type { components } from '@/api/schema'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import type { UploadAttributes } from './upload-client'

export type UploadSchema = components['schemas']['UploadSchema']
export type UploadField = components['schemas']['UploadField']

export interface PickedFile {
  /** Stable key for the row, so removing one file does not shift the typed values of another. */
  id: string
  file: File
}

export type FieldValue = string | boolean

export interface UploadFormState {
  /** Values of the component scope fields, by field name. */
  fields: Readonly<Record<string, FieldValue>>
  files: readonly PickedFile[]
  /** Values of the asset scope fields per file id. */
  assetFields: Readonly<Record<string, Readonly<Record<string, FieldValue>>>>
}

export interface UploadErrors {
  /** One message about the files as a whole (too few, too many). */
  files?: string
  /** Messages by file id. */
  file: Readonly<Record<string, string>>
  fields: Readonly<Record<string, string>>
  assetFields: Readonly<Record<string, Readonly<Record<string, string>>>>
}

export const noErrors: UploadErrors = { file: {}, fields: {}, assetFields: {} }

export function hasErrors(errors: UploadErrors): boolean {
  return (
    errors.files !== undefined ||
    Object.keys(errors.file).length > 0 ||
    Object.keys(errors.fields).length > 0 ||
    Object.keys(errors.assetFields).length > 0
  )
}

export function fieldsOf(schema: UploadSchema, scope: UploadField['scope']): UploadField[] {
  return schema.fields.filter((f) => f.scope === scope)
}

function primitiveText(value: unknown): string | null {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
    return String(value)
  }
  return null
}

export function initialValue(field: UploadField): FieldValue {
  if (field.type === 'boolean') return field.default === true
  const fallback = field.type === 'enum' ? (field.enum_values?.[0] ?? '') : ''
  if (field.default === undefined || field.default === null) return fallback
  return primitiveText(field.default) ?? fallback
}

export function initialFields(schema: UploadSchema): Record<string, FieldValue> {
  return Object.fromEntries(fieldsOf(schema, 'component').map((f) => [f.name, initialValue(f)]))
}

function hasAllowedExtension(name: string, allowed: readonly string[]): boolean {
  if (allowed.length === 0) return true
  const lower = name.toLowerCase()
  return allowed.some((ext) => lower.endsWith(ext.toLowerCase()))
}

function checkField(field: UploadField, value: FieldValue | undefined): string | undefined {
  const text = typeof value === 'string' ? value.trim() : ''
  if (field.type === 'boolean') return undefined
  // A field the server can read from the uploaded file is not required of the user.
  const derivable = field.derive_from !== null && field.derive_from !== undefined
  if (text === '') {
    return field.required && !derivable
      ? t('upload.errors.required', { label: field.label })
      : undefined
  }
  if (field.type === 'integer' && !/^-?\d+$/.test(text)) {
    return t('upload.errors.integer', { label: field.label })
  }
  if (field.pattern) {
    try {
      if (!new RegExp(field.pattern).test(text)) {
        return t('upload.errors.pattern', { label: field.label })
      }
    } catch {
      // A pattern the browser cannot compile is a hint only; the server enforces it anyway.
    }
  }
  return undefined
}

/**
 * Client side checks of the form, from the upload schema the server sent. They only save a round
 * trip: the server validates everything again (docs/api-conventions.md), and its field errors are
 * shown on the same fields.
 */
export function validate(schema: UploadSchema, state: UploadFormState): UploadErrors {
  const fields: Record<string, string> = {}
  for (const field of fieldsOf(schema, 'component')) {
    const message = checkField(field, state.fields[field.name])
    if (message) fields[field.name] = message
  }
  const file: Record<string, string> = {}
  for (const { id, file: picked } of state.files) {
    if (!hasAllowedExtension(picked.name, schema.accepted_extensions)) {
      file[id] = t('upload.errors.extension', { allowed: schema.accepted_extensions.join(', ') })
    } else if (schema.max_file_bytes > 0 && picked.size > schema.max_file_bytes) {
      file[id] = t('upload.errors.tooLarge', { max: formatBytes(schema.max_file_bytes) })
    }
  }
  const assetFields: Record<string, Record<string, string>> = {}
  for (const { id } of state.files) {
    for (const field of fieldsOf(schema, 'asset')) {
      const message = checkField(field, state.assetFields[id]?.[field.name])
      if (message) assetFields[id] = { ...assetFields[id], [field.name]: message }
    }
  }
  let files: string | undefined
  if (state.files.length < Math.max(1, schema.min_files)) files = t('upload.errors.noFiles')
  else if (schema.max_files > 0 && state.files.length > schema.max_files) {
    files = t('upload.errors.tooManyFiles', { max: schema.max_files })
  }
  return { files, file, fields, assetFields }
}

function cast(field: UploadField, value: FieldValue | undefined): unknown {
  if (field.type === 'boolean') return value === true
  const text = typeof value === 'string' ? value.trim() : ''
  if (text === '') return undefined
  return field.type === 'integer' ? Number(text) : text
}

/** The `attributes` part of the multipart request: component fields once, asset fields per file in order. */
export function buildAttributes(schema: UploadSchema, state: UploadFormState): UploadAttributes {
  const fields: Record<string, unknown> = {}
  for (const field of fieldsOf(schema, 'component')) {
    const value = cast(field, state.fields[field.name])
    if (value !== undefined) fields[field.name] = value
  }
  const assetSchema = fieldsOf(schema, 'asset')
  return {
    fields,
    assets: state.files.map(({ id }) => {
      const values: Record<string, unknown> = {}
      for (const field of assetSchema) {
        const value = cast(field, state.assetFields[id]?.[field.name])
        if (value !== undefined) values[field.name] = value
      }
      return { fields: values, sha256: null }
    }),
  }
}

/**
 * The files go out one after another in one request body, so the bytes sent so far say how far
 * each file is: the first files are complete, the next is partial, the rest not started.
 */
export function fileProgress(
  sizes: readonly number[],
  sentBytes: number,
  total: number,
  loaded: number,
): number[] {
  const done = total > 0 && loaded >= total
  let remaining = Math.min(
    sentBytes,
    sizes.reduce((sum, size) => sum + size, 0),
  )
  return sizes.map((size) => {
    if (done || size === 0) return done || remaining > 0 ? 1 : 0
    const part = Math.max(0, Math.min(size, remaining))
    remaining -= part
    return part / size
  })
}

/** `/fields/group_id` and `/assets/0/fields/extension` from the server mapped onto the form. */
export function mapServerErrors(
  fields: readonly { field: string; message: string }[],
  files: readonly PickedFile[],
): UploadErrors {
  const componentErrors: Record<string, string> = {}
  const assetErrors: Record<string, Record<string, string>> = {}
  let general: string | undefined
  for (const { field, message } of fields) {
    const asset = /^\/assets\/(\d+)\/fields\/(.+)$/.exec(field)
    const component = /^\/fields\/(.+)$/.exec(field)
    const picked = asset?.[1] === undefined ? undefined : files[Number(asset[1])]
    if (asset?.[2] !== undefined && picked !== undefined) {
      assetErrors[picked.id] = { ...assetErrors[picked.id], [asset[2]]: message }
    } else if (component?.[1] !== undefined) componentErrors[component[1]] = message
    else general = message
  }
  return { files: general, file: {}, fields: componentErrors, assetFields: assetErrors }
}
