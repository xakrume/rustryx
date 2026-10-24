import { useQueryClient } from '@tanstack/react-query'
import { ConflictDialog } from '@/components/conflict-dialog'
import { Form, TextField, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  blobStoreKeys,
  fetchBlobStore,
  patchBlobStore,
  type BlobStorePatch,
  type BlobStoreView,
} from './api'
import { applyBlobStoreErrors, BlobStoreFields } from './blob-store-fields'
import {
  buildPatch,
  editSchema,
  redactPatch,
  valuesFromView,
  type BlobStoreFormValues,
} from './form-model'

/**
 * Edit form of the detail page. It is filled from the stored view, which holds no credentials,
 * so a secret can only ever be in the form because the user typed it in this session; after a
 * save the form is reset to the server's view and the typed values are gone. The save sends the
 * ETag that was read with the object (`If-Match`): a stale one opens the conflict dialog.
 */
export function ConfigForm({ view }: { view: BlobStoreView }) {
  const queryClient = useQueryClient()
  const lockedBy = view.repository_count
  const form = useZodForm(editSchema(lockedBy > 0), { defaultValues: valuesFromView(view) })

  const update = useUpdateResource<BlobStoreView, BlobStorePatch>({
    queryKey: blobStoreKeys.detail(view.name),
    resourceName: `blob store ${view.name}`,
    update: (patch, etag) => patchBlobStore(view.name, patch, etag),
    fetchCurrent: () => fetchBlobStore(view.name),
    invalidate: [blobStoreKeys.all],
    // The copy never carries credential values: a secret must not reach the clipboard by accident.
    copyChanges: redactPatch,
    onSuccess: (saved) => {
      form.reset(valuesFromView(saved))
      toast.success(t('blobStores.toast.saved', { name: saved.name }))
      void queryClient.invalidateQueries({ queryKey: blobStoreKeys.usage(saved.name) })
    },
    onError: (error) => {
      if (!applyBlobStoreErrors(form, error)) toast.apiError(error, view.name)
    },
  })

  function onSubmit(values: BlobStoreFormValues): void {
    const patch = buildPatch(view, values)
    if (Object.keys(patch).length === 0) {
      form.reset(valuesFromView(view))
      return
    }
    update.mutate(patch)
  }

  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-2">
        <CardTitle>{t('blobStores.sections.configuration')}</CardTitle>
        <Badge>{t(`blobStores.kind.${view.kind}`)}</Badge>
      </CardHeader>
      <CardContent>
        <UnsavedChangesGuard when={form.formState.isDirty && !update.isPending} />
        <Form form={form} onSubmit={onSubmit} aria-label={t('blobStores.sections.configuration')}>
          <div className="sm:max-w-sm">
            <TextField<BlobStoreFormValues>
              name="name"
              label={t('blobStores.fields.name')}
              helper={t('blobStores.locked.name')}
              readOnly
              className="font-mono"
            />
          </div>
          <BlobStoreFields mode="edit" lockedBy={lockedBy} credentialsState={view.credentials} />
          <div className="flex flex-wrap gap-2">
            <Button type="submit" loading={update.isPending} disabled={!form.formState.isDirty}>
              {update.isPending ? t('blobStores.actions.saving') : t('blobStores.actions.save')}
            </Button>
            <Button
              variant="outline"
              disabled={!form.formState.isDirty || update.isPending}
              onClick={() => form.reset(valuesFromView(view))}
            >
              {t('blobStores.actions.discard')}
            </Button>
          </div>
        </Form>
        <ConflictDialog {...update.conflict} />
      </CardContent>
    </Card>
  )
}
