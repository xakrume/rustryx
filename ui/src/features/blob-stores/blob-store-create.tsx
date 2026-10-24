import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { Form, SelectField, TextField, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { PageHeader } from '@/components/page-header'
import { Button } from '@/components/ui/button'
import { useSearchPatch } from '@/features/system/search-params'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { createBlobStore, blobStoreKeys } from './api'
import { applyBlobStoreErrors, BlobStoreFields } from './blob-store-fields'
import { createSchema, EMPTY_VALUES, toCreateBody, type BlobStoreFormValues } from './form-model'

/**
 * Create view of the blob store list (`/admin/blob-stores?view=create`): kind first, then the
 * fields of that kind. Credentials are typed here and sent once; nothing of them is kept in the
 * form after the save (the form is reset before the redirect) and the server never returns them.
 */
export function BlobStoreCreateView() {
  const patchSearch = useSearchPatch()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const form = useZodForm(createSchema, { defaultValues: EMPTY_VALUES })
  const [failure, setFailure] = useState<unknown>(null)

  const create = useMutation({
    mutationFn: createBlobStore,
    // Failures are shown on the fields or in the form, not as a second toast.
    meta: { silent: true },
  })

  async function onSubmit(values: BlobStoreFormValues): Promise<void> {
    setFailure(null)
    try {
      const created = await create.mutateAsync(toCreateBody(values))
      // Clear every typed value, secrets included, BEFORE leaving: the unsaved changes guard
      // must not block the redirect and no credential may linger in a form that stays mounted.
      form.reset(EMPTY_VALUES)
      await queryClient.invalidateQueries({ queryKey: blobStoreKeys.all })
      toast.success(t('blobStores.toast.created', { name: created.name }))
      void navigate({ to: '/admin/blob-stores/$name', params: { name: created.name } })
    } catch (error) {
      if (!applyBlobStoreErrors(form, error)) setFailure(error)
    }
  }

  return (
    <>
      <PageHeader
        title={t('blobStores.createTitle')}
        description={t('blobStores.createDescription')}
        actions={
          <Button variant="outline" onClick={() => patchSearch({ view: undefined })}>
            {t('blobStores.backToList')}
          </Button>
        }
      />
      <UnsavedChangesGuard when={form.formState.isDirty && !create.isPending} />
      <Form
        form={form}
        onSubmit={onSubmit}
        className="max-w-2xl"
        aria-label={t('blobStores.createTitle')}
      >
        <div className="sm:max-w-sm">
          <TextField<BlobStoreFormValues>
            name="name"
            label={t('blobStores.fields.name')}
            helper={t('blobStores.fields.nameHelper')}
            required
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </div>
        <div className="sm:max-w-sm">
          <SelectField<BlobStoreFormValues>
            name="kind"
            label={t('blobStores.fields.kind')}
            helper={t('blobStores.fields.kindHelper')}
            required
          >
            <option value="local">{t('blobStores.kind.local')}</option>
            <option value="s3">{t('blobStores.kind.s3')}</option>
          </SelectField>
        </div>
        <BlobStoreFields mode="create" />
        {failure !== null ? <ErrorPanel error={failure} /> : null}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" loading={create.isPending}>
            {t('blobStores.create')}
          </Button>
          <Button variant="outline" onClick={() => patchSearch({ view: undefined })}>
            {t('common.cancel')}
          </Button>
        </div>
      </Form>
    </>
  )
}
