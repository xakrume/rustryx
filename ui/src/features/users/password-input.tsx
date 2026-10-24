import { Eye, EyeOff } from 'lucide-react'
import { useState } from 'react'
import { useFormContext, get, type FieldValues, type Path } from 'react-hook-form'
import { FormField } from '@/components/form/form-field'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { t } from '@/i18n'

export interface PasswordInputProps<T extends FieldValues> {
  name: Path<T>
  label: string
  helper?: string
  required?: boolean
  /** `new-password` (default) keeps password managers from filling an old one into a reset form. */
  autoComplete?: 'new-password' | 'current-password'
}

/**
 * A password field of a react-hook-form form with a show and hide toggle (a pressed button, so
 * the state is announced). The value lives only in the form state: nothing is stored, logged or
 * rendered as text unless the user chooses to reveal it.
 */
export function PasswordInput<T extends FieldValues>({
  name,
  label,
  helper,
  required,
  autoComplete = 'new-password',
}: PasswordInputProps<T>) {
  const [shown, setShown] = useState(false)
  const { register, formState } = useFormContext<T>()
  const message: unknown = (get(formState.errors, name) as { message?: unknown } | undefined)
    ?.message
  return (
    <FormField
      label={label}
      helper={helper}
      required={required}
      error={typeof message === 'string' ? message : undefined}
    >
      {(control) => (
        <div className="relative">
          <Input
            {...control}
            type={shown ? 'text' : 'password'}
            autoComplete={autoComplete}
            spellCheck={false}
            className="pr-10"
            {...register(name)}
          />
          <Button
            variant="ghost"
            size="icon"
            className="absolute top-0 right-0"
            aria-label={t(shown ? 'users.reset.hide' : 'users.reset.show', {
              what: label.toLowerCase(),
            })}
            aria-pressed={shown}
            onClick={() => setShown((v) => !v)}
          >
            {shown ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
          </Button>
        </div>
      )}
    </FormField>
  )
}
