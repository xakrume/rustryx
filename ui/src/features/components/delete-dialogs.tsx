import { isApiError } from '@/api/errors'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { useDeleteAsset, useDeleteComponent } from './api'

/** A delete that finds nothing is done (the contract says to treat a repeated DELETE's 404 as success). */
async function tolerateGone(action: () => Promise<unknown>): Promise<void> {
  try {
    await action()
  } catch (error) {
    if (!(isApiError(error) && error.status === 404)) throw error
  }
}

export interface DeleteAssetDialogProps {
  asset: { id: string; path: string } | null
  onClose: () => void
  onDeleted?: () => void
}

/**
 * Deleting a file names its path. The blob stays until garbage collection removes it, which the
 * text says, because "deleted" must not promise that the bytes are gone from disk. A failure
 * (409 immutable, 403, 5xx) stays inside the dialog with its code and request id.
 */
export function DeleteAssetDialog({ asset, onClose, onDeleted }: DeleteAssetDialogProps) {
  const remove = useDeleteAsset()
  if (asset === null) return null
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      kind={t('components.kind.asset')}
      name={asset.path}
      description={t('components.deleteAssetBody')}
      onConfirm={async () => {
        await tolerateGone(() => remove.mutateAsync(asset.id))
        toast.success(t('components.assetDeleted', { path: asset.path }))
        onDeleted?.()
      }}
    />
  )
}

export interface DeleteComponentDialogProps {
  component: { id: string; label: string; assets: number } | null
  onClose: () => void
  onDeleted?: () => void
}

export function DeleteComponentDialog({
  component,
  onClose,
  onDeleted,
}: DeleteComponentDialogProps) {
  const remove = useDeleteComponent()
  if (component === null) return null
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      kind={t('components.kind.component')}
      name={component.label}
      description={t('components.deleteComponentBody', { count: component.assets })}
      onConfirm={async () => {
        await tolerateGone(() => remove.mutateAsync(component.id))
        toast.success(t('components.componentDeleted', { name: component.label }))
        onDeleted?.()
      }}
    />
  )
}
