import { useBlocker } from '@tanstack/react-router'
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

/**
 * Warns before leaving a form with unsaved edits, by in-app navigation (a dialog) and by
 * closing or reloading the tab (the browser's own prompt). Render it next to the form with
 * `when={form.formState.isDirty}`; clear the dirty state (form.reset) after a successful save
 * BEFORE navigating away, or the guard blocks the redirect that follows the save.
 */
export function UnsavedChangesGuard({ when }: { when: boolean }) {
  const blocker = useBlocker({
    shouldBlockFn: () => when,
    enableBeforeUnload: () => when,
    withResolver: true,
  })
  const blocked = blocker.status === 'blocked'
  return (
    <Dialog
      open={blocked}
      onOpenChange={(open) => !open && blocker.status === 'blocked' && blocker.reset()}
    >
      <DialogContent role="alertdialog">
        <DialogHeader>
          <DialogTitle>{t('form.unsavedTitle')}</DialogTitle>
          <DialogDescription>{t('form.unsavedBody')}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="destructive"
            onClick={() => blocker.status === 'blocked' && blocker.proceed()}
          >
            {t('form.leave')}
          </Button>
          <Button data-autofocus="" onClick={() => blocker.status === 'blocked' && blocker.reset()}>
            {t('form.stay')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
