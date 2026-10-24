import { X } from 'lucide-react'
import { BytesText } from '@/components/bytes-text'
import { FormField, InlineField } from '@/components/form/form-field'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import { FileDrop } from './file-drop'
import {
  fieldsOf,
  initialValue,
  type FieldValue,
  type PickedFile,
  type UploadErrors,
  type UploadField,
  type UploadFormState,
  type UploadSchema,
} from './upload-model'

function helperOf(field: UploadField): string | undefined {
  const parts = [
    field.description ?? undefined,
    field.derive_from ? t(`upload.derive.${field.derive_from}`) : undefined,
  ].filter((p): p is string => p !== undefined)
  return parts.length > 0 ? parts.join(' ') : undefined
}

interface FieldInputProps {
  field: UploadField
  value: FieldValue | undefined
  error: string | undefined
  disabled: boolean
  onChange: (value: FieldValue) => void
}

/** One input of the schema driven form, by the field's type; label, helper and error are wired for assistive tech. */
function FieldInput({ field, value, error, disabled, onChange }: FieldInputProps) {
  const required = field.required && !field.derive_from
  if (field.type === 'boolean') {
    return (
      <InlineField layout="checkbox" label={field.label} helper={helperOf(field)} error={error}>
        {(c) => (
          <Checkbox
            id={c.id}
            aria-describedby={c['aria-describedby']}
            checked={value === true}
            disabled={disabled}
            onCheckedChange={(checked) => onChange(checked === true)}
          />
        )}
      </InlineField>
    )
  }
  return (
    <FormField label={field.label} helper={helperOf(field)} error={error} required={required}>
      {(c) =>
        field.type === 'enum' ? (
          <Select
            {...c}
            value={String(value ?? initialValue(field))}
            disabled={disabled}
            onChange={(e) => onChange(e.target.value)}
          >
            {(field.enum_values ?? []).map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </Select>
        ) : (
          <Input
            {...c}
            value={String(value ?? '')}
            disabled={disabled}
            inputMode={field.type === 'integer' ? 'numeric' : undefined}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => onChange(e.target.value)}
          />
        )
      }
    </FormField>
  )
}

export interface UploadFormProps {
  schema: UploadSchema
  state: UploadFormState
  errors: UploadErrors
  disabled: boolean
  onChange: (next: UploadFormState) => void
}

let counter = 0
const nextId = (): string => `file-${(counter += 1)}`

/**
 * The form of one format, generated from its upload schema: component fields once, a file area,
 * and the asset fields once per file. Adding a file to a single file format replaces the previous
 * one; the extension, size and count limits of the schema are checked on submit.
 */
export function UploadForm({ schema, state, errors, disabled, onChange }: UploadFormProps) {
  const componentFields = fieldsOf(schema, 'component')
  const assetFields = fieldsOf(schema, 'asset')

  function addFiles(files: File[]): void {
    const picked: PickedFile[] = files.map((file) => ({ id: nextId(), file }))
    if (picked.length === 0) return
    onChange({
      ...state,
      files: schema.multi_file ? [...state.files, ...picked] : picked.slice(0, 1),
    })
  }

  function removeFile(id: string): void {
    onChange({ ...state, files: state.files.filter((f) => f.id !== id) })
  }

  return (
    <div className="space-y-5">
      {componentFields.length > 0 ? (
        <section
          aria-labelledby="upload-fields-title"
          className="space-y-4 rounded-lg border border-border bg-card p-4"
        >
          <h2 id="upload-fields-title" className="text-base font-semibold">
            {t('upload.detailsTitle')}
          </h2>
          <div className="grid gap-4 sm:grid-cols-2">
            {componentFields.map((field) => (
              <FieldInput
                key={field.name}
                field={field}
                value={state.fields[field.name]}
                error={errors.fields[field.name]}
                disabled={disabled}
                onChange={(value) =>
                  onChange({ ...state, fields: { ...state.fields, [field.name]: value } })
                }
              />
            ))}
          </div>
        </section>
      ) : null}

      <section
        aria-labelledby="upload-files-title"
        className="space-y-3 rounded-lg border border-border bg-card p-4"
      >
        <h2 id="upload-files-title" className="text-base font-semibold">
          {schema.multi_file ? t('upload.filesTitle') : t('upload.fileTitle')}
        </h2>
        <FileDrop
          multiple={schema.multi_file}
          accept={schema.accepted_extensions.join(',')}
          disabled={disabled}
          hint={[
            schema.accepted_extensions.length > 0
              ? t('upload.accepts', { extensions: schema.accepted_extensions.join(', ') })
              : t('upload.acceptsAny'),
            t('upload.maxSize', { size: formatBytes(schema.max_file_bytes) }),
            schema.multi_file ? t('upload.maxFiles', { count: schema.max_files }) : '',
          ]
            .filter(Boolean)
            .join(' · ')}
          onFiles={addFiles}
        />
        {errors.files ? (
          <p role="alert" className="text-xs font-medium text-destructive-text">
            {errors.files}
          </p>
        ) : null}
        {state.files.length > 0 ? (
          <ul aria-label={t('upload.selectedFiles')} className="m-0 list-none space-y-2 p-0">
            {state.files.map(({ id, file }) => (
              <li key={id} className="rounded-md border border-border p-3">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="min-w-0 flex-1 font-mono text-sm break-all">{file.name}</span>
                  <BytesText bytes={file.size} className="text-sm text-muted-foreground" />
                  <Button
                    variant="ghost"
                    size="icon"
                    disabled={disabled}
                    aria-label={t('upload.removeFile', { name: file.name })}
                    onClick={() => removeFile(id)}
                  >
                    <X aria-hidden="true" />
                  </Button>
                </div>
                {errors.file[id] ? (
                  <p role="alert" className="mt-1 text-xs font-medium text-destructive-text">
                    {errors.file[id]}
                  </p>
                ) : null}
                {assetFields.length > 0 ? (
                  <div className="mt-3 grid gap-3 sm:grid-cols-2">
                    {assetFields.map((field) => (
                      <FieldInput
                        key={field.name}
                        field={{ ...field, label: `${field.label} (${file.name})` }}
                        value={state.assetFields[id]?.[field.name]}
                        error={errors.assetFields[id]?.[field.name]}
                        disabled={disabled}
                        onChange={(value) =>
                          onChange({
                            ...state,
                            assetFields: {
                              ...state.assetFields,
                              [id]: { ...state.assetFields[id], [field.name]: value },
                            },
                          })
                        }
                      />
                    ))}
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        ) : null}
      </section>
    </div>
  )
}
