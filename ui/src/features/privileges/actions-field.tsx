import { CircleAlert } from 'lucide-react'
import { useId } from 'react'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'
import { ACTIONS, type Action } from './api'

export interface ActionsFieldProps {
  value: readonly Action[]
  onChange: (next: Action[]) => void
  error?: string
  disabled?: boolean
}

/** The four actions as checkboxes in a labelled group; a privilege needs at least one. */
export function ActionsField({ value, onChange, error, disabled = false }: ActionsFieldProps) {
  const base = useId()
  const messageId = `${base}-message`
  return (
    <fieldset
      className="space-y-2"
      disabled={disabled}
      aria-describedby={messageId}
      aria-invalid={error ? true : undefined}
    >
      <legend className="text-sm font-medium">
        {t('privileges.editor.actions')}
        <span aria-hidden="true" className="ml-0.5 text-destructive-text">
          *
        </span>
        <span className="sr-only"> ({t('form.required')})</span>
      </legend>
      <div className="flex flex-wrap gap-x-6 gap-y-2">
        {ACTIONS.map((action) => (
          <div key={action} className="flex items-center gap-2">
            <Checkbox
              id={`${base}-${action}`}
              checked={value.includes(action)}
              onCheckedChange={(checked) =>
                onChange(
                  checked === true
                    ? ACTIONS.filter((a) => a === action || value.includes(a))
                    : value.filter((a) => a !== action),
                )
              }
            />
            <Label htmlFor={`${base}-${action}`}>{t(`identity.action.${action}`)}</Label>
          </div>
        ))}
      </div>
      <div id={messageId} className="space-y-1">
        <p className="text-xs text-muted-foreground">{t('privileges.editor.actionsHelper')}</p>
        {error ? (
          <p className="flex items-start gap-1 text-xs font-medium text-destructive-text">
            <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
            <span>{error}</span>
          </p>
        ) : null}
      </div>
    </fieldset>
  )
}
