import { useRef, useState } from 'react'
import { isApiError } from '@/api/errors'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { useDeleteRepository } from './api'

export interface DeleteRepositoryDialogProps {
  /** The repository to delete. Null keeps the dialog closed. */
  name: string | null
  onClose: () => void
  onDeleted?: () => void
}

interface Stage {
  includeContent: boolean
  components: number | null
}

/**
 * Deleting a repository, as the contract stages it (docs/api-conventions.md, 18 decision 14).
 * First a plain delete with the typed name. If the repository still holds components the server
 * answers 409 `repository_not_empty` (the count is in `details.component_count`); the SAME
 * dialog then says what the content is and asks again, and only that second confirmation sends
 * `include_content=true`. A repository that a group still uses answers 409 `resource_in_use`:
 * the shared confirm dialog lists the groups and nothing is deleted.
 */
export function DeleteRepositoryDialog({ name, onClose, onDeleted }: DeleteRepositoryDialogProps) {
  const remove = useDeleteRepository()
  const [stage, setStage] = useState<Stage>({ includeContent: false, components: null })
  // The confirm dialog closes itself after `onConfirm` resolves. The switch to the second stage
  // resolves too, but must keep the dialog open: this flag tells the close handler to stay.
  const stayOpen = useRef(false)

  if (name === null) return null
  const description = stage.includeContent
    ? stage.components === null
      ? t('repos.delete.withContentUnknown', { name })
      : t('repos.delete.withContent', { name, count: stage.components })
    : t('repos.delete.body', { name })

  async function confirm(): Promise<void> {
    if (name === null) return
    try {
      await remove.mutateAsync({ name, includeContent: stage.includeContent })
    } catch (error) {
      if (isApiError(error) && error.code === 'repository_not_empty' && !stage.includeContent) {
        const count = error.details?.component_count
        stayOpen.current = true
        setStage({ includeContent: true, components: typeof count === 'number' ? count : null })
        return
      }
      // A repeated delete after success is a 404, which the contract says to treat as done.
      if (!(isApiError(error) && error.status === 404)) throw error
    }
    toast.success(t('repos.delete.deleted', { name }))
    onDeleted?.()
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => {
        if (open) return
        if (stayOpen.current) {
          stayOpen.current = false
          return
        }
        setStage({ includeContent: false, components: null })
        onClose()
      }}
      kind={t('repos.kind')}
      name={name}
      description={description}
      confirmLabel={stage.includeContent ? t('repos.delete.confirmWithContent') : undefined}
      requireTyping
      onConfirm={confirm}
    />
  )
}
