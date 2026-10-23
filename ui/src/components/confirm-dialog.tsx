import { useState } from 'react'
import { isApiError, type Dependent } from '@/api/errors'
import { ErrorPanel } from '@/components/error-panel'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'

export interface ConfirmDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Kind of the object, lower case: "repository", "role", "token". */
  kind: string
  /** Name of the object. Shown in the title and, with `requireTyping`, typed by the user. */
  name: string
  /** Replaces the default "This will permanently delete ..." sentence. */
  description?: string
  confirmLabel?: string
  /** Irreversible actions: the confirm button stays disabled until `name` is typed exactly. */
  requireTyping?: boolean
  /** Objects that depend on this one, usually from a 409 `resource_in_use` (`details.dependents`). */
  dependents?: readonly Dependent[]
  /** Runs on confirm. A thrown error stays in the dialog; success closes it. */
  onConfirm: () => unknown
}

/**
 * Confirmation for destructive actions: names the object in the title, lists what depends on
 * it, and for irreversible actions asks to type the exact name (docs/ui-design-brief.md rule 4).
 * It is an alertdialog and Cancel (the safe choice) takes the initial focus; with typing
 * required the input does, because Enter must not be able to confirm by accident.
 */
export function ConfirmDialog(props: ConfirmDialogProps) {
  return (
    <Dialog open={props.open} onOpenChange={props.onOpenChange}>
      <ConfirmContent {...props} />
    </Dialog>
  )
}

function ConfirmContent({
  onOpenChange,
  kind,
  name,
  description,
  confirmLabel,
  requireTyping = false,
  dependents = [],
  onConfirm,
}: ConfirmDialogProps) {
  const [typed, setTyped] = useState('')
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const matches = !requireTyping || typed === name

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

  // A 409 from the action itself carries the blocking objects: show them where the user is.
  const blockers = dependents.length > 0 ? dependents : isApiError(error) ? error.dependents : []

  return (
    <DialogContent role="alertdialog" dismissible={!pending}>
      <DialogHeader>
        <DialogTitle>
          {t('dialog.deleteTitle', { kind })} <span className="font-mono break-all">{name}</span>?
        </DialogTitle>
        <DialogDescription>
          {description ?? t('dialog.confirmDeleteBody', { kind, name })}
          {requireTyping ? ` ${t('dialog.irreversible')}` : ''}
        </DialogDescription>
      </DialogHeader>

      {blockers.length > 0 ? (
        <div className="mb-3 text-sm">
          <p className="font-medium">{t('dialog.dependents')}</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {blockers.map((d) => (
              <li key={`${d.kind}-${d.id ?? d.name}`}>
                <span className="font-mono">{d.name}</span>
                {d.kind ? <span className="text-muted-foreground"> ({d.kind})</span> : null}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {requireTyping ? (
        <div className="space-y-1">
          <Label htmlFor="confirm-typed">{t('dialog.typeToConfirm', { name })}</Label>
          <Input
            id="confirm-typed"
            data-autofocus=""
            value={typed}
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            aria-label={t('dialog.typeToConfirmLabel')}
            onChange={(e) => setTyped(e.target.value)}
          />
        </div>
      ) : null}

      {error !== null ? <ErrorPanel className="mt-3" error={error} /> : null}

      <DialogFooter>
        <Button
          variant="outline"
          data-autofocus={requireTyping ? undefined : ''}
          disabled={pending}
          onClick={() => onOpenChange(false)}
        >
          {t('common.cancel')}
        </Button>
        <Button
          variant="destructive"
          disabled={!matches}
          loading={pending}
          onClick={() => void confirm()}
        >
          {confirmLabel ?? t('common.delete')}
        </Button>
      </DialogFooter>
    </DialogContent>
  )
}
