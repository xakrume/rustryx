import { Link } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { Controller, get, useFormContext } from 'react-hook-form'
import { FormField } from '@/components/form/form-field'
import { SelectField, SwitchField, TextField } from '@/components/form/form'
import { Alert } from '@/components/ui/alert'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { t } from '@/i18n'
import type { BlobStoreView, CleanupPolicyView, FormatInfo, RoutingRuleView } from '../api'
import { humanize, type ConfigField } from './format-config'
import type { RepositoryFormValues } from './values'

type Values = RepositoryFormValues

export function FormSection({
  title,
  description,
  children,
}: {
  title: string
  description?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="space-y-4 rounded-xl border border-transparent bg-card dark:border-border/60 p-4">
      <div>
        <h2 className="text-base leading-6 font-semibold">{title}</h2>
        {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
      </div>
      {children}
    </section>
  )
}

const WRITE_POLICIES = ['allow', 'allow_once', 'deny'] as const

export interface StorageSectionProps {
  mode: 'create' | 'edit'
  blobStores: readonly BlobStoreView[]
  format: FormatInfo | undefined
  /** The name of the blob store of an existing repository: the view carries it, so no list lookup is needed. */
  lockedBlobStore?: string
}

/** Where content lives and what a repeated upload does. The blob store is fixed after creation. */
export function StorageSection({ mode, blobStores, format, lockedBlobStore }: StorageSectionProps) {
  const { watch } = useFormContext<Values>()
  const repoType = watch('repo_type')
  const policy = watch('write_policy')
  const blobStoreId = watch('blob_store_id')
  const storeName =
    lockedBlobStore ?? blobStores.find((s) => s.id === blobStoreId)?.name ?? blobStoreId
  return (
    <FormSection title={t('repoForm.storage.title')} description={t('repoForm.storage.body')}>
      {mode === 'edit' ? (
        <FormField
          label={t('repoForm.storage.blobStore')}
          helper={t('repoForm.fixedAfterCreation')}
        >
          {(control) => <Input {...control} value={storeName} readOnly className="font-mono" />}
        </FormField>
      ) : (
        <SelectField<Values>
          name="blob_store_id"
          label={t('repoForm.storage.blobStore')}
          helper={
            blobStores.length === 0 ? (
              <>
                {t('repoForm.storage.noBlobStores')}{' '}
                <Link to="/admin/blob-stores" className="underline underline-offset-2">
                  {t('repoForm.storage.createBlobStore')}
                </Link>
              </>
            ) : (
              t('repoForm.storage.blobStoreHelper')
            )
          }
          required
        >
          <option value="">{t('repoForm.storage.choose')}</option>
          {blobStores.map((store) => (
            <option key={store.id} value={store.id}>
              {store.name} ({store.kind})
            </option>
          ))}
        </SelectField>
      )}
      {repoType === 'hosted' ? (
        <SelectField<Values>
          name="write_policy"
          label={t('repoForm.storage.writePolicy')}
          helper={t(`repoForm.storage.policyHelp.${policy}`)}
          required
        >
          {WRITE_POLICIES.map((value) => (
            <option key={value} value={value}>
              {t(`repoForm.storage.policy.${value}`)}
              {format?.default_write_policy === value ? ` (${t('repoForm.storage.default')})` : ''}
            </option>
          ))}
        </SelectField>
      ) : null}
      <TextField<Values>
        name="quota_gib"
        label={t('repoForm.storage.quota')}
        helper={t('repoForm.storage.quotaHelper')}
        inputMode="decimal"
        className="block max-w-48"
      />
    </FormSection>
  )
}

export function AccessSection() {
  const { watch } = useFormContext<Values>()
  return (
    <FormSection title={t('repoForm.access.title')} description={t('repoForm.access.body')}>
      <SwitchField<Values>
        name="allow_anonymous"
        label={t('repoForm.access.anonymous')}
        helper={t('repoForm.access.anonymousHelper')}
      />
      {watch('allow_anonymous') ? (
        <Alert variant="warning">{t('repoForm.access.anonymousWarning')}</Alert>
      ) : null}
      <SwitchField<Values>
        name="online"
        label={t('repoForm.access.online')}
        helper={t('repoForm.access.onlineHelper')}
      />
    </FormSection>
  )
}

function criterion(label: string, value: number | string | null | undefined): string | null {
  return value === null || value === undefined ? null : label.replace('{value}', String(value))
}

/** One line of what a cleanup policy removes, from the criteria that are set. */
export function criteriaSummary(criteria: CleanupPolicyView['criteria']): string {
  const parts = [
    criterion(t('repoForm.policies.olderThan'), criteria.older_than_days),
    criterion(t('repoForm.policies.notDownloaded'), criteria.last_downloaded_days),
    criterion(t('repoForm.policies.keepLast'), criteria.keep_last_versions),
    criterion(t('repoForm.policies.namePattern'), criteria.name_pattern),
    criterion(t('repoForm.policies.releaseType'), criteria.release_type),
  ].filter((p): p is string => p !== null)
  return parts.join(', ')
}

export interface PoliciesSectionProps {
  policies: readonly CleanupPolicyView[]
  rules: readonly RoutingRuleView[]
}

/** Cleanup policies of this format, and for proxies and groups the routing rule. */
export function PoliciesSection({ policies, rules }: PoliciesSectionProps) {
  const { watch, setValue } = useFormContext<Values>()
  const format = watch('format')
  const repoType = watch('repo_type')
  const attached = watch('cleanup_policy_ids')
  const matching = policies.filter((p) => p.format === format)

  function toggle(id: string, checked: boolean): void {
    const next = checked ? [...attached, id] : attached.filter((existing) => existing !== id)
    setValue('cleanup_policy_ids', next, { shouldDirty: true, shouldTouch: true })
  }

  return (
    <FormSection title={t('repoForm.policies.title')} description={t('repoForm.policies.body')}>
      <fieldset className="space-y-2">
        <legend className="mb-1 text-sm leading-none font-medium">
          {t('repoForm.policies.cleanup')}
        </legend>
        {matching.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {t('repoForm.policies.none')}{' '}
            <Link to="/admin/cleanup-policies" className="underline underline-offset-2">
              {t('repoForm.policies.manage')}
            </Link>
          </p>
        ) : (
          matching.map((policy) => (
            <div key={policy.id} className="flex items-start gap-2">
              <Checkbox
                id={`policy-${policy.id}`}
                className="mt-0.5"
                checked={attached.includes(policy.id)}
                onCheckedChange={(checked) => toggle(policy.id, checked === true)}
              />
              <Label htmlFor={`policy-${policy.id}`} className="flex flex-col leading-5">
                <span>{policy.name}</span>
                <span className="text-xs font-normal text-muted-foreground">
                  {criteriaSummary(policy.criteria)}
                </span>
              </Label>
            </div>
          ))
        )}
      </fieldset>
      {repoType !== 'hosted' ? (
        <SelectField<Values>
          name="routing_rule_id"
          label={t('repoForm.policies.routingRule')}
          helper={t('repoForm.policies.routingRuleHelper')}
        >
          <option value="">{t('common.none')}</option>
          {rules.map((rule) => (
            <option key={rule.id} value={rule.id}>
              {rule.name} ({rule.mode})
            </option>
          ))}
        </SelectField>
      ) : null}
    </FormSection>
  )
}

export function FormatOptionsSection({
  fields,
  label,
}: {
  fields: readonly ConfigField[]
  label: string
}) {
  const { control, formState } = useFormContext<Values>()
  if (fields.length === 0) return null
  return (
    <FormSection
      title={t('repoForm.options.title', { format: label })}
      description={t('repoForm.options.body')}
    >
      {fields.map((field) => {
        const name = `config.${field.key}` as const
        const error = (get(formState.errors, name) as { message?: string } | undefined)?.message
        return (
          <Controller
            key={field.key}
            control={control}
            name={name}
            render={({ field: rhf }) => (
              <FormField
                label={field.label}
                helper={field.description ?? undefined}
                error={typeof error === 'string' ? error : undefined}
              >
                {(c) =>
                  field.kind === 'boolean' ? (
                    <Switch
                      id={c.id}
                      aria-describedby={c['aria-describedby']}
                      checked={rhf.value === true}
                      onCheckedChange={rhf.onChange}
                      ref={rhf.ref}
                    />
                  ) : field.kind === 'enum' ? (
                    <Select
                      {...c}
                      value={String(rhf.value ?? '')}
                      onChange={(event) => rhf.onChange(event.target.value)}
                      onBlur={rhf.onBlur}
                      ref={rhf.ref}
                      className="block max-w-64"
                    >
                      {field.options.map((option) => (
                        <option key={option} value={option}>
                          {humanize(option)}
                        </option>
                      ))}
                    </Select>
                  ) : (
                    <Input
                      {...c}
                      value={String(rhf.value ?? '')}
                      inputMode={field.kind === 'integer' ? 'numeric' : undefined}
                      onChange={(event) => rhf.onChange(event.target.value)}
                      onBlur={rhf.onBlur}
                      ref={rhf.ref}
                      className="block max-w-64"
                    />
                  )
                }
              </FormField>
            )}
          />
        )
      })}
    </FormSection>
  )
}
