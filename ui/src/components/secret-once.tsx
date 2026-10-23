import { useEffect, useState } from 'react'
import { CodeBlock } from '@/components/code-block'
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

export interface SecretOnceProps {
  /**
   * The secret to show, or null when there is none (the dialog is closed). Keep it in the
   * state of the caller only until `onDone`: never in the query cache, a router search
   * param or storage. The server never returns it again.
   */
  secret: string | null
  /** What it is, for the title and the copy button: "API token", "webhook secret". */
  what: string
  /** Called when the user closed the dialog after acknowledging. The caller must drop the secret. */
  onDone: () => void
}

/**
 * Show-once secret dialog (docs/ui-page-inventory.md, section 5). The dialog cannot be closed
 * with Escape, a click outside or the close button until the user ticks "I stored it", so a
 * stray click cannot lose the only copy of a token.
 */
export function SecretOnce({ secret, what, onDone }: SecretOnceProps) {
  const [acknowledged, setAcknowledged] = useState(false)
  // A new secret starts unacknowledged; so does the next one after the dialog closed.
  useEffect(() => {
    setAcknowledged(false)
  }, [secret])
  return (
    <Dialog
      open={secret !== null}
      // Escape and the corner button only reach here once acknowledged (dismissible below).
      onOpenChange={(open) => {
        if (!open && acknowledged) onDone()
      }}
    >
      <SecretOnceContent
        secret={secret}
        what={what}
        onDone={onDone}
        acknowledged={acknowledged}
        onAcknowledgedChange={setAcknowledged}
      />
    </Dialog>
  )
}

interface ContentProps extends SecretOnceProps {
  acknowledged: boolean
  onAcknowledgedChange: (value: boolean) => void
}

function SecretOnceContent({
  secret,
  what,
  onDone,
  acknowledged,
  onAcknowledgedChange,
}: ContentProps) {
  return (
    <DialogContent dismissible={acknowledged} hideCloseButton={!acknowledged}>
      <DialogHeader>
        <DialogTitle>{t('secret.title', { what })}</DialogTitle>
        <DialogDescription>{t('secret.body')}</DialogDescription>
      </DialogHeader>
      <CodeBlock value={secret ?? ''} label={what} wrap />
      <div className="mt-4 flex items-center gap-2">
        <Checkbox
          id="secret-ack"
          checked={acknowledged}
          onCheckedChange={(checked) => onAcknowledgedChange(checked === true)}
        />
        <Label htmlFor="secret-ack">{t('secret.acknowledge')}</Label>
      </div>
      {!acknowledged ? (
        <p className="mt-2 text-xs text-muted-foreground">{t('secret.mustAcknowledge')}</p>
      ) : null}
      <DialogFooter>
        <Button onClick={onDone} disabled={!acknowledged}>
          {t('secret.done')}
        </Button>
      </DialogFooter>
    </DialogContent>
  )
}
