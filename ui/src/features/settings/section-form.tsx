import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { isApiError } from '@/api/errors'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import {
  Form,
  pointerToName,
  SelectField,
  SwitchField,
  TextField,
  useZodForm,
} from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { RelativeTime } from '@/components/relative-time'
import { scalarText } from '@/features/system/scalar-text'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  fetchSection,
  sectionQuery,
  settingsKeys,
  settingsSchemaQuery,
  updateSection,
  type SectionName,
  type SettingsSection,
} from './api'
import { ReadOnlyModeDialog } from './read-only-dialog'
import {
  buildSchema,
  changedValues,
  fieldsOf,
  valuesFromSection,
  type FieldSpec,
  type SectionFormValues,
} from './schema-model'

const SOURCE_BADGE = { config: 'neutral', runtime: 'info', default: 'neutral' } as const
const SOURCE_KEY = {
  config: 'systemSettings.source.config',
  runtime: 'systemSettings.source.runtime',
  default: 'systemSettings.source.default',
} as const

function SourceBadge({ source }: { source: string | undefined }) {
  const known = source === 'config' || source === 'runtime' || source === 'default'
  if (!known) return null
  return <Badge variant={SOURCE_BADGE[source]}>{t(SOURCE_KEY[source])}</Badge>
}

const isSecretFlag = (key: string): boolean => key.endsWith('_set')

/** A value that is not editable: shown as text with its source, secrets only as set or not set. */
function ReadOnlyRow({
  field,
  value,
  source,
}: {
  field: FieldSpec
  value: unknown
  source?: string
}) {
  const text = isSecretFlag(field.key)
    ? t(value === true ? 'systemSettings.values.configured' : 'systemSettings.values.notConfigured')
    : field.type === 'boolean'
      ? t(value === true ? 'systemSettings.values.on' : 'systemSettings.values.off')
      : scalarText(value) === ''
        ? t('systemSettings.values.empty')
        : scalarText(value)
  return (
    <div className="grid gap-1 py-3 sm:grid-cols-[minmax(0,16rem)_minmax(0,1fr)] sm:gap-4">
      <dt className="text-sm font-medium">
        {field.title}
        <span className="sr-only">: {t('systemSettings.readOnly')}</span>
      </dt>
      <dd className="space-y-1">
        <p className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-sm break-all">{text}</span>
          <SourceBadge source={source} />
        </p>
        {field.description ? (
          <p className="text-xs text-muted-foreground">{field.description}</p>
        ) : null}
        {source === 'config' ? (
          <p className="text-xs text-muted-foreground">{t('systemSettings.source.configNote')}</p>
        ) : null}
      </dd>
    </div>
  )
}

function EditableRow({ field, source }: { field: FieldSpec; source?: string }) {
  const label = (
    <span className="inline-flex flex-wrap items-center gap-2">
      {field.title}
      <SourceBadge source={source} />
    </span>
  )
  const helper = field.description ?? undefined
  if (field.type === 'boolean') {
    return <SwitchField<SectionFormValues> name={field.key} label={label} helper={helper} />
  }
  if (field.enumValues !== null) {
    return (
      <div className="sm:max-w-xs">
        <SelectField<SectionFormValues> name={field.key} label={label} helper={helper}>
          {field.enumValues.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </SelectField>
      </div>
    )
  }
  return (
    <div className="sm:max-w-xs">
      <TextField<SectionFormValues>
        name={field.key}
        label={label}
        helper={helper}
        required
        inputMode={field.type === 'string' ? undefined : 'numeric'}
        autoComplete="off"
      />
    </div>
  )
}

interface BodyProps {
  name: SectionName
  section: SettingsSection
  fields: readonly FieldSpec[]
}

function SectionFormBody({ name, section, fields }: BodyProps) {
  const initial = useMemo(() => valuesFromSection(fields, section), [fields, section])
  const schema = useMemo(() => buildSchema(fields, section.editable_fields), [fields, section])
  const form = useZodForm(schema, { defaultValues: initial })
  const [pendingValues, setPendingValues] = useState<Record<string, unknown> | null>(null)

  const update = useUpdateResource<SettingsSection, Record<string, unknown>>({
    queryKey: settingsKeys.section(name),
    resourceName: t('systemSettings.conflictResource', { section: name }),
    update: (values, etag) => updateSection(name, values, etag),
    fetchCurrent: () => fetchSection(name),
    copyChanges: (values) => JSON.stringify(values, null, 2),
    onSuccess: (saved) => {
      form.reset(valuesFromSection(fields, saved))
      toast.success(t('systemSettings.saved'))
    },
    onError: (error) => {
      if (isApiError(error) && error.fieldErrors.length > 0) {
        error.fieldErrors.forEach((f, index) => {
          const key = pointerToName(f.field).replace(/^values\./, '')
          form.setError(key, { type: 'server', message: f.message }, { shouldFocus: index === 0 })
        })
        return
      }
      toast.apiError(error, name)
    },
  })

  const editable = section.editable_fields
  const specs = (keys: readonly string[]) => fields.filter((f) => keys.includes(f.key))
  const readOnlyFields = fields.filter((f) => !editable.includes(f.key))

  function onSubmit(values: SectionFormValues): void {
    const changed = changedValues(fields, editable, initial, values)
    if (Object.keys(changed).length === 0) {
      toast.info(t('systemSettings.noChanges'))
      return
    }
    // Turning the system read only changes what every user can do: say so and ask first.
    if (changed.read_only_mode === true) {
      setPendingValues(changed)
      return
    }
    update.mutate(changed)
  }

  const dirty = form.formState.isDirty
  const changedBy = section.updated_by?.username
  return (
    <div className="space-y-4">
      <UnsavedChangesGuard when={dirty && !update.isPending} />
      <p className="text-sm text-muted-foreground">
        {section.updated_at && changedBy ? (
          <>
            {t('systemSettings.lastChanged', { user: changedBy })}{' '}
            <RelativeTime value={section.updated_at} />
          </>
        ) : (
          t('systemSettings.neverChanged')
        )}
      </p>
      {editable.length > 0 ? (
        <Form
          form={form}
          onSubmit={onSubmit}
          className="max-w-3xl"
          aria-label={t(`systemSettings.sections.${name}`)}
        >
          {specs(editable).map((field) => (
            <EditableRow key={field.key} field={field} source={section.sources[field.key]} />
          ))}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" loading={update.isPending} disabled={!dirty}>
              {update.isPending ? t('systemSettings.saving') : t('systemSettings.save')}
            </Button>
            <Button
              variant="outline"
              disabled={!dirty || update.isPending}
              onClick={() => form.reset(initial)}
            >
              {t('systemSettings.discard')}
            </Button>
          </div>
        </Form>
      ) : (
        <Alert variant="info">{t('systemSettings.noEditable')}</Alert>
      )}
      {readOnlyFields.length > 0 ? (
        <section aria-label={t('systemSettings.readOnly')}>
          <h2 className="text-sm font-semibold">{t('systemSettings.readOnly')}</h2>
          <dl className="divide-y divide-border">
            {readOnlyFields.map((field) => (
              <ReadOnlyRow
                key={field.key}
                field={field}
                value={section.values[field.key]}
                source={section.sources[field.key]}
              />
            ))}
          </dl>
          {readOnlyFields.some((f) => isSecretFlag(f.key)) ? (
            <p className="text-xs text-muted-foreground">{t('systemSettings.secretNote')}</p>
          ) : null}
        </section>
      ) : null}
      <ConflictDialog {...update.conflict} />
      <ReadOnlyModeDialog
        open={pendingValues !== null}
        onCancel={() => setPendingValues(null)}
        onConfirm={() => {
          if (pendingValues !== null) update.mutate(pendingValues)
          setPendingValues(null)
        }}
      />
    </div>
  )
}

/** One section of the settings: the form of its editable fields and the read only rest. */
export function SectionForm({ name }: { name: SectionName }) {
  const schema = useQuery(settingsSchemaQuery)
  const section = useQuery(sectionQuery(name))
  const fields = useMemo(() => fieldsOf(schema.data?.sections[name]), [schema.data, name])
  const error = section.error ?? schema.error
  if ((section.isError && !section.data) || (schema.isError && !schema.data)) {
    return (
      <ErrorPanel
        error={error}
        onRetry={() => {
          void section.refetch()
          void schema.refetch()
        }}
      />
    )
  }
  if (!section.data || !schema.data) return <Skeleton className="h-48 max-w-3xl" aria-busy="true" />
  return (
    // Keyed by the ETag: a reload or a save rebuilds the form from the server's object.
    <SectionFormBody
      key={section.data.etag ?? name}
      name={name}
      section={section.data.data}
      fields={fields}
    />
  )
}
