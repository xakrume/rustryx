import { useFormContext } from 'react-hook-form'
import { CheckboxField, SelectField, TextField, pointerToName } from '@/components/form/form'
import { isApiError } from '@/api/errors'
import { t } from '@/i18n'
import type { UseFormSetError } from 'react-hook-form'
import type { BlobStoreView } from './api'
import type { BlobStoreFormValues } from './form-model'

export interface BlobStoreFieldsProps {
  mode: 'create' | 'edit'
  /** Repositories use the store: path, bucket and endpoint are shown but cannot change. */
  lockedBy?: number
  credentialsState?: BlobStoreView['credentials']
}

/** Map the contract's JSON pointers onto the form's field names. */
const POINTERS: Readonly<Record<string, keyof BlobStoreFormValues>> = {
  '/config/path': 'path',
  '/config/endpoint': 'endpoint',
  '/config/bucket': 'bucket',
  '/config/region': 'region',
  '/config/key_prefix': 'keyPrefix',
  '/config/path_style': 'pathStyle',
  '/config/part_size_mb': 'partSizeMb',
  '/quota_bytes': 'quotaGiB',
  '/credentials/access_key': 'accessKey',
  '/credentials/secret_key': 'secretKey',
  '/credentials/access_key_ref': 'accessKeyRef',
  '/credentials/secret_key_ref': 'secretKeyRef',
  '/name': 'name',
  '/kind': 'kind',
}

/**
 * Put a failed save onto the fields it names and focus the first. Returns false when the error
 * has nothing to show next to a field (the caller shows it as a message). A refused S3 endpoint
 * (`upstream_url_rejected`) and a taken name (`already_exists`) belong to one field each.
 */
export function applyBlobStoreErrors(
  form: { setError: UseFormSetError<BlobStoreFormValues> },
  error: unknown,
): boolean {
  if (!isApiError(error)) return false
  if (error.code === 'already_exists') {
    form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
    return true
  }
  if (error.code === 'upstream_url_rejected') {
    const rule = error.details?.rule
    const message = typeof rule === 'string' ? `${error.message} (${rule})` : error.message
    form.setError('endpoint', { type: 'server', message }, { shouldFocus: true })
    return true
  }
  const fields = error.fieldErrors
  if (fields.length === 0) return false
  fields.forEach((f, index) => {
    const name = POINTERS[f.field] ?? (pointerToName(f.field) as keyof BlobStoreFormValues)
    form.setError(name, { type: 'server', message: f.message }, { shouldFocus: index === 0 })
  })
  return true
}

function LockedNote({ count }: { count: number }) {
  return <p className="text-xs text-muted-foreground">{t('blobStores.locked.config', { count })}</p>
}

function LocalFields({ lockedBy }: { lockedBy: number }) {
  return (
    <>
      <TextField<BlobStoreFormValues>
        name="path"
        label={t('blobStores.fields.path')}
        helper={t('blobStores.fields.pathHelper')}
        required
        readOnly={lockedBy > 0}
        spellCheck={false}
        autoComplete="off"
        className="font-mono"
      />
      {lockedBy > 0 ? <LockedNote count={lockedBy} /> : null}
    </>
  )
}

function S3Fields({ lockedBy }: { lockedBy: number }) {
  const locked = lockedBy > 0
  return (
    <>
      <TextField<BlobStoreFormValues>
        name="endpoint"
        label={t('blobStores.fields.endpoint')}
        helper={t('blobStores.fields.endpointHelper')}
        required
        readOnly={locked}
        inputMode="url"
        spellCheck={false}
        autoComplete="off"
        className="font-mono"
      />
      <div className="grid gap-4 sm:grid-cols-2">
        <TextField<BlobStoreFormValues>
          name="bucket"
          label={t('blobStores.fields.bucket')}
          required
          readOnly={locked}
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
        <TextField<BlobStoreFormValues>
          name="region"
          label={t('blobStores.fields.region')}
          required
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
        <TextField<BlobStoreFormValues>
          name="keyPrefix"
          label={t('blobStores.fields.keyPrefix')}
          helper={t('blobStores.fields.keyPrefixHelper')}
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
        <TextField<BlobStoreFormValues>
          name="partSizeMb"
          label={t('blobStores.fields.partSize')}
          helper={t('blobStores.fields.partSizeHelper')}
          inputMode="numeric"
          autoComplete="off"
        />
      </div>
      {locked ? <LockedNote count={lockedBy} /> : null}
      <CheckboxField<BlobStoreFormValues>
        name="pathStyle"
        label={t('blobStores.fields.pathStyle')}
        helper={t('blobStores.fields.pathStyleHelper')}
      />
    </>
  )
}

function CredentialFields({
  mode,
  credentialsState,
}: Pick<BlobStoreFieldsProps, 'mode' | 'credentialsState'>) {
  const { watch } = useFormContext<BlobStoreFormValues>()
  const source = watch('credentialSource')
  const edit = mode === 'edit'
  return (
    <fieldset className="space-y-3 rounded-lg border border-border p-4">
      <legend className="px-1 text-sm font-semibold">{t('blobStores.credentials.legend')}</legend>
      {edit && credentialsState ? (
        <p className="text-sm">
          {t('blobStores.credentials.state', {
            state: t(
              credentialsState.configured
                ? 'blobStores.credentials.stateConfigured'
                : 'blobStores.credentials.stateNone',
            ),
          })}
          {credentialsState.configured
            ? `, ${t(`blobStores.credentials.sourceLabel.${credentialsState.source}`)}`
            : ''}
          {credentialsState.access_key_ref && credentialsState.secret_key_ref ? (
            <span className="mt-1 block font-mono text-xs text-muted-foreground">
              {t('blobStores.credentials.references', {
                access: credentialsState.access_key_ref,
                secret: credentialsState.secret_key_ref,
              })}
            </span>
          ) : null}
        </p>
      ) : null}
      <SelectField<BlobStoreFormValues>
        name="credentialSource"
        label={t('blobStores.credentials.source')}
        helper={edit ? t('blobStores.credentials.writeOnlyNote') : undefined}
      >
        <option value="default">
          {t(edit ? 'blobStores.credentials.sourceKeep' : 'blobStores.credentials.sourceDefault')}
        </option>
        <option value="inline">
          {t(
            edit
              ? 'blobStores.credentials.sourceReplaceInline'
              : 'blobStores.credentials.sourceInline',
          )}
        </option>
        <option value="reference">
          {t(
            edit
              ? 'blobStores.credentials.sourceReplaceReference'
              : 'blobStores.credentials.sourceReference',
          )}
        </option>
      </SelectField>
      {source === 'inline' ? (
        <>
          <p className="text-xs text-muted-foreground">
            {t('blobStores.credentials.inlineHelper')}
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <TextField<BlobStoreFormValues>
              name="accessKey"
              label={t('blobStores.credentials.accessKey')}
              required
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
            <TextField<BlobStoreFormValues>
              name="secretKey"
              label={t('blobStores.credentials.secretKey')}
              required
              type="password"
              autoComplete="new-password"
              spellCheck={false}
              className="font-mono"
            />
          </div>
        </>
      ) : null}
      {source === 'reference' ? (
        <>
          <p className="text-xs text-muted-foreground">{t('blobStores.credentials.refHelper')}</p>
          <div className="grid gap-4 sm:grid-cols-2">
            <TextField<BlobStoreFormValues>
              name="accessKeyRef"
              label={t('blobStores.credentials.accessKeyRef')}
              required
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="env:S3_ACCESS_KEY"
            />
            <TextField<BlobStoreFormValues>
              name="secretKeyRef"
              label={t('blobStores.credentials.secretKeyRef')}
              required
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="env:S3_SECRET_KEY"
            />
          </div>
        </>
      ) : null}
    </fieldset>
  )
}

/** The kind specific part of the blob store form, shared by the create view and the detail page. */
export function BlobStoreFields({ mode, lockedBy = 0, credentialsState }: BlobStoreFieldsProps) {
  const { watch } = useFormContext<BlobStoreFormValues>()
  const kind = watch('kind')
  return (
    <>
      {kind === 'local' ? <LocalFields lockedBy={lockedBy} /> : <S3Fields lockedBy={lockedBy} />}
      {kind === 's3' ? <CredentialFields mode={mode} credentialsState={credentialsState} /> : null}
      <div className="sm:max-w-sm">
        <TextField<BlobStoreFormValues>
          name="quotaGiB"
          label={t('blobStores.fields.quota')}
          helper={t('blobStores.fields.quotaHelper')}
          inputMode="decimal"
          autoComplete="off"
        />
      </div>
    </>
  )
}
