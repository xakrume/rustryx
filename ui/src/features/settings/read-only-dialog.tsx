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

export interface ReadOnlyModeDialogProps {
  open: boolean
  onCancel: () => void
  onConfirm: () => void
}

/**
 * Turning on read only mode refuses writes for every user (uploads, deletes, settings), so it
 * is confirmed with its effect spelled out. The safe answer, keeping it off, takes the focus.
 */
export function ReadOnlyModeDialog({ open, onCancel, onConfirm }: ReadOnlyModeDialogProps) {
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent role="alertdialog">
        <DialogHeader>
          <DialogTitle>{t('systemSettings.readOnlyMode.confirmTitle')}</DialogTitle>
          <DialogDescription>{t('systemSettings.readOnlyMode.confirmBody')}</DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">{t('systemSettings.readOnlyMode.audited')}</p>
        <DialogFooter>
          <Button variant="outline" data-autofocus="" onClick={onCancel}>
            {t('systemSettings.readOnlyMode.cancel')}
          </Button>
          <Button variant="destructive" onClick={onConfirm}>
            {t('systemSettings.readOnlyMode.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
