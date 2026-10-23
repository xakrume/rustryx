import { CircleAlert } from 'lucide-react'
import { useId, type ReactNode } from 'react'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

/** Props to spread on the control so label, helper and error are wired for assistive tech. */
export interface FieldControlProps {
  id: string
  'aria-describedby': string | undefined
  'aria-invalid': true | undefined
  'aria-required': true | undefined
}

interface FieldShellProps {
  /** Always visible. A placeholder is never a label (WCAG 3.3.2). */
  label: ReactNode
  helper?: ReactNode
  /** Inline error text next to the field; an icon and the text carry it, not color alone. */
  error?: string
  required?: boolean
  /** Fixed id for the control, for tests and anchors. Generated when omitted. */
  id?: string
  className?: string
  children: (control: FieldControlProps) => ReactNode
}

/** Ids and aria wiring shared by every field layout. */
function useFieldWiring({
  id,
  helper,
  error,
  required,
}: Pick<FieldShellProps, 'id' | 'helper' | 'error' | 'required'>) {
  const generated = useId()
  const controlId = id ?? generated
  const helperId = `${controlId}-helper`
  const errorId = `${controlId}-error`
  const describedBy = [helper ? helperId : null, error ? errorId : null].filter(Boolean).join(' ')
  const control: FieldControlProps = {
    id: controlId,
    'aria-describedby': describedBy || undefined,
    'aria-invalid': error ? true : undefined,
    'aria-required': required ? true : undefined,
  }
  return { control, helperId, errorId }
}

function RequiredMark() {
  return (
    <>
      <span aria-hidden="true" className="ml-0.5 text-destructive-text">
        *
      </span>
      <span className="sr-only"> ({t('form.required')})</span>
    </>
  )
}

function Messages({
  helper,
  error,
  helperId,
  errorId,
  className,
}: {
  helper?: ReactNode
  error?: string
  helperId: string
  errorId: string
  className?: string
}) {
  return (
    <>
      {helper ? (
        <p id={helperId} className={cn('text-xs text-muted-foreground', className)}>
          {helper}
        </p>
      ) : null}
      {error ? (
        <p
          id={errorId}
          className={cn(
            'flex items-start gap-1 text-xs font-medium text-destructive-text',
            className,
          )}
        >
          <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
    </>
  )
}

/**
 * The shell around any control: visible label with a required marker, helper text, inline
 * error. The control comes in through a render prop so the same shell serves Input, Select,
 * Textarea or a custom widget, and always gets matching id, aria-describedby and aria-invalid.
 */
export function FormField(props: FieldShellProps) {
  const { label, helper, error, required, className, children } = props
  const { control, helperId, errorId } = useFieldWiring(props)
  return (
    <div className={cn('space-y-1.5', className)}>
      <Label htmlFor={control.id}>
        {label}
        {required ? <RequiredMark /> : null}
      </Label>
      {children(control)}
      <Messages helper={helper} error={error} helperId={helperId} errorId={errorId} />
    </div>
  )
}

/**
 * Field whose control sits on the same row as its label: a checkbox (box first, label after it)
 * or a switch (label first, switch at the end of the row). Helper and error go underneath.
 */
export function InlineField(props: FieldShellProps & { layout: 'checkbox' | 'switch' }) {
  const { label, helper, error, required, className, layout, children } = props
  const { control, helperId, errorId } = useFieldWiring(props)
  const labelNode = (
    <Label htmlFor={control.id} className="leading-5">
      {label}
      {required ? <RequiredMark /> : null}
    </Label>
  )
  return (
    <div className={cn('space-y-1', className)}>
      {layout === 'checkbox' ? (
        <div className="flex items-start gap-2">
          <span className="mt-0.5">{children(control)}</span>
          {labelNode}
        </div>
      ) : (
        <div className="flex items-center justify-between gap-3">
          {labelNode}
          {children(control)}
        </div>
      )}
      <Messages
        helper={helper}
        error={error}
        helperId={helperId}
        errorId={errorId}
        className={layout === 'checkbox' ? 'pl-6' : undefined}
      />
    </div>
  )
}
