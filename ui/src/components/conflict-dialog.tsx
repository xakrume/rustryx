import type { ApiError } from '@/api/errors'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { t } from '@/i18n'

export interface ConflictDialogProps {
  open: boolean
  /**
   * `stale` is 412 `precondition_failed`: someone changed the object since it was read, so
   * the user chooses between reloading, overwriting and keeping a copy of their edit.
   * `state` is 409: the request conflicts with the current state whatever the version (name
   * taken, still in use). There is nothing to reload, the dialog explains and lists blockers.
   */
  kind: 'stale' | 'state'
  /** Names the object: "role maven-publishers". */
  resourceName: string
  error: ApiError | null
  pending?: boolean
  onReload: () => void
  /** Present only for `stale`: repeat the update against the fresh version. */
  onOverwrite?: () => void
  /** Present when the caller can serialize the user's edit to the clipboard. */
  onCopyMine?: () => void
  onClose: () => void
}

/** The three way answer to a lost update (docs/api-conventions.md, 5.3 and 8). */
export function ConflictDialog({
  open,
  kind,
  resourceName,
  error,
  pending = false,
  onReload,
  onOverwrite,
  onCopyMine,
  onClose,
}: ConflictDialogProps) {
  const stale = kind === 'stale'
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent role="alertdialog" dismissible={!pending}>
        <DialogHeader>
          <DialogTitle>
            {stale ? t('errors.conflictDialog.staleTitle') : t('errors.conflictDialog.stateTitle')}
          </DialogTitle>
          <DialogDescription>
            {stale
              ? t('errors.conflictDialog.staleBody', { name: resourceName })
              : (error?.message ?? '')}
          </DialogDescription>
        </DialogHeader>

        {!stale && error && error.dependents.length > 0 ? (
          <div className="text-sm">
            <p className="font-medium">{t('errors.conflictDialog.blockedBy')}</p>
            <ul className="mt-1 list-disc space-y-0.5 pl-5">
              {error.dependents.map((d) => (
                <li key={`${d.kind}-${d.id ?? d.name}`} className="font-mono">
                  {d.name}
                </li>
              ))}
            </ul>
          </div>
        ) : null}

        {error?.requestId ? (
          <p className="mt-2 text-xs text-muted-foreground">
            {t('common.requestId')}: <span className="font-mono">{error.requestId}</span>
          </p>
        ) : null}

        <DialogFooter>
          {stale && onCopyMine ? (
            <Button variant="ghost" onClick={onCopyMine} disabled={pending}>
              {t('errors.conflictDialog.copyMine')}
            </Button>
          ) : null}
          {stale && onOverwrite ? (
            <Button variant="outline" onClick={onOverwrite} loading={pending}>
              {t('errors.conflictDialog.overwrite')}
            </Button>
          ) : null}
          {stale ? (
            <Button data-autofocus="" onClick={onReload} disabled={pending}>
              {t('errors.conflictDialog.reload')}
            </Button>
          ) : (
            <Button data-autofocus="" onClick={onClose}>
              {t('common.close')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
