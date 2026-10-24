import { useState, type ReactNode } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'

export interface ActionConfirmDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Names the object and the action: "Disable user bob?". */
  title: string
  description: ReactNode
  confirmLabel: string
  variant?: 'default' | 'destructive'
  /** An extra tick box that must be set before the action is possible ("I understand"). */
  acknowledge?: string
  /** Runs on confirm. A thrown error stays in the dialog; success closes it. */
  onConfirm: () => unknown
  children?: ReactNode
}

/**
 * Confirmation for an action that is not a delete (disable an account, end sessions, expose
 * repositories): the title names the object, the safe choice (Cancel) takes the first focus,
 * and a failure stays in the dialog with its code and request id. `ConfirmDialog` is for deletes
 * and says "Delete" in its title, so it cannot serve here.
 */
export function ActionConfirmDialog(props: ActionConfirmDialogProps) {
  return (
    <Dialog open={props.open} onOpenChange={props.onOpenChange}>
      <Content {...props} />
    </Dialog>
  )
}

function Content({
  onOpenChange,
  title,
  description,
  confirmLabel,
  variant = 'default',
  acknowledge,
  onConfirm,
  children,
}: ActionConfirmDialogProps) {
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const [acknowledged, setAcknowledged] = useState(false)

  async function confirm(): Promise<void> {
    setPending(true)
    setError(null)
    try {
      await onConfirm()
      onOpenChange(false)
    } catch (e) {
      setError(e)
    } finally {
      setPending(false)
    }
  }

  return (
    <DialogContent role="alertdialog" dismissible={!pending}>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
      </DialogHeader>
      {children}
      {acknowledge ? (
        <div className="mt-3 flex items-start gap-2">
          <Checkbox
            id="action-ack"
            className="mt-0.5"
            checked={acknowledged}
            onCheckedChange={(checked) => setAcknowledged(checked === true)}
          />
          <Label htmlFor="action-ack" className="leading-5">
            {acknowledge}
          </Label>
        </div>
      ) : null}
      {error !== null ? <ErrorPanel className="mt-3" error={error} /> : null}
      <DialogFooter>
        <Button
          variant="outline"
          data-autofocus=""
          disabled={pending}
          onClick={() => onOpenChange(false)}
        >
          {t('common.cancel')}
        </Button>
        <Button
          variant={variant}
          loading={pending}
          disabled={acknowledge !== undefined && !acknowledged}
          onClick={() => void confirm()}
        >
          {confirmLabel}
        </Button>
      </DialogFooter>
    </DialogContent>
  )
}
