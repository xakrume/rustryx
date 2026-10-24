import { Link } from '@tanstack/react-router'
import { ConfirmDialog } from '@/components/confirm-dialog'
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
import { toast } from '@/lib/toast-store'
import { useDeleteSelector, type SelectorView } from './api'

/**
 * A selector that no privilege uses is deleted after a confirmation that names it. One that is in
 * use cannot be deleted at all (the contract has no `force` for selectors), so instead of an
 * action that is certain to fail, the dialog says why and links to the privileges to change first.
 */
export function SelectorDeleteDialog({
  selector,
  onClose,
}: {
  selector: SelectorView | null
  onClose: () => void
}) {
  if (selector === null) return null
  return selector.used_by_privileges.length > 0 ? (
    <InUse selector={selector} onClose={onClose} />
  ) : (
    <Confirm selector={selector} onClose={onClose} />
  )
}

function InUse({ selector, onClose }: { selector: SelectorView; onClose: () => void }) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent role="alertdialog">
        <DialogHeader>
          <DialogTitle>{t('selectors.delete.blockedTitle', { name: selector.name })}</DialogTitle>
          <DialogDescription>{t('selectors.delete.blockedBody')}</DialogDescription>
        </DialogHeader>
        <div className="text-sm">
          <p className="font-medium">{t('selectors.delete.blockedList')}</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {selector.used_by_privileges.map((p) => (
              <li key={p.id}>
                <Link
                  to="/admin/privileges"
                  search={{ edit: p.id }}
                  className="font-mono text-link underline underline-offset-2"
                >
                  {p.name}
                </Link>
              </li>
            ))}
          </ul>
        </div>
        <DialogFooter>
          <Button data-autofocus="" onClick={onClose}>
            {t('common.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function Confirm({ selector, onClose }: { selector: SelectorView; onClose: () => void }) {
  const remove = useDeleteSelector()
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      kind={t('selectors.delete.kind')}
      name={selector.name}
      confirmLabel={t('selectors.delete.confirm')}
      onConfirm={async () => {
        await remove.mutateAsync(selector.id)
        toast.success(t('selectors.delete.deleted', { name: selector.name }))
      }}
    />
  )
}
