import '@/lib/zod-setup'
import { zodResolver } from '@hookform/resolvers/zod'
import type { ComponentProps, FormEvent, ReactNode } from 'react'
import {
  Controller,
  FormProvider,
  get,
  useForm,
  useFormContext,
  type FieldValues,
  type Path,
  type UseFormProps,
  type UseFormReturn,
} from 'react-hook-form'
import type { z } from 'zod'
import { isApiError } from '@/api/errors'
import { FormField, InlineField } from '@/components/form/form-field'
import { Checkbox } from '@/components/ui/checkbox'
import { Input, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { cn } from '@/lib/utils'

/**
 * react-hook-form with a zod schema. `mode: 'onTouched'` validates a field when the user leaves
 * it and then on every change, so errors appear next to the field at the moment they are
 * useful and clear as soon as they are fixed. The first invalid field receives focus on submit.
 */
export function useZodForm<TIn extends FieldValues, TOut extends FieldValues = TIn>(
  schema: z.ZodType<TOut, TIn>,
  options: Omit<UseFormProps<TIn, unknown, TOut>, 'resolver'> = {},
): UseFormReturn<TIn, unknown, TOut> {
  return useForm<TIn, unknown, TOut>({
    mode: 'onTouched',
    ...options,
    resolver: zodResolver(schema),
  })
}

export interface FormProps<TIn extends FieldValues, TOut extends FieldValues> extends Omit<
  ComponentProps<'form'>,
  'onSubmit'
> {
  form: UseFormReturn<TIn, unknown, TOut>
  onSubmit: (values: TOut) => void | Promise<void>
  children: ReactNode
}

export function Form<TIn extends FieldValues, TOut extends FieldValues>({
  form,
  onSubmit,
  children,
  className,
  ...props
}: FormProps<TIn, TOut>) {
  return (
    <FormProvider {...form}>
      <form
        noValidate
        className={cn('space-y-4', className)}
        onSubmit={(event: FormEvent<HTMLFormElement>) => void form.handleSubmit(onSubmit)(event)}
        {...props}
      >
        {children}
      </form>
    </FormProvider>
  )
}

/** `/proxy/remote_url` (JSON pointer of the API) to `proxy.remote_url` (react-hook-form path). */
export function pointerToName(pointer: string): string {
  return pointer
    .replace(/^\//, '')
    .split('/')
    .map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'))
    .join('.')
}

/**
 * Map the field errors of an API failure (`details.fields`, docs/api-conventions.md 5.1) onto
 * the form and focus the first one. Returns false when the error carries none, so the caller
 * falls back to a toast. A pointer that matches no field still lands in `errors` and is
 * reachable through `FormRootError`, never silently dropped.
 */
export function applyApiFieldErrors<TIn extends FieldValues, TOut extends FieldValues>(
  form: UseFormReturn<TIn, unknown, TOut>,
  error: unknown,
): boolean {
  if (!isApiError(error)) return false
  const fields = error.fieldErrors
  if (fields.length === 0) return false
  fields.forEach((f, index) => {
    form.setError(
      pointerToName(f.field) as Path<TIn>,
      { type: 'server', message: f.message },
      { shouldFocus: index === 0 },
    )
  })
  return true
}

interface BoundProps<T extends FieldValues> {
  name: Path<T>
  label: ReactNode
  helper?: ReactNode
  required?: boolean
}

function fieldError<T extends FieldValues>(errors: object, name: Path<T>): string | undefined {
  const message: unknown = (get(errors, name) as { message?: unknown } | undefined)?.message
  return typeof message === 'string' ? message : undefined
}

export function TextField<T extends FieldValues>({
  name,
  label,
  helper,
  required,
  ...input
}: BoundProps<T> & Omit<ComponentProps<typeof Input>, 'name' | 'required'>) {
  const { register, formState } = useFormContext<T>()
  return (
    <FormField
      label={label}
      helper={helper}
      required={required}
      error={fieldError(formState.errors, name)}
    >
      {(control) => <Input {...control} {...input} {...register(name)} />}
    </FormField>
  )
}

export function TextareaField<T extends FieldValues>({
  name,
  label,
  helper,
  required,
  ...input
}: BoundProps<T> & Omit<ComponentProps<typeof Textarea>, 'name' | 'required'>) {
  const { register, formState } = useFormContext<T>()
  return (
    <FormField
      label={label}
      helper={helper}
      required={required}
      error={fieldError(formState.errors, name)}
    >
      {(control) => <Textarea {...control} {...input} {...register(name)} />}
    </FormField>
  )
}

export function SelectField<T extends FieldValues>({
  name,
  label,
  helper,
  required,
  children,
  ...select
}: BoundProps<T> & Omit<ComponentProps<typeof Select>, 'name' | 'required'>) {
  const { register, formState } = useFormContext<T>()
  return (
    <FormField
      label={label}
      helper={helper}
      required={required}
      error={fieldError(formState.errors, name)}
    >
      {(control) => (
        <Select {...control} {...select} {...register(name)}>
          {children}
        </Select>
      )}
    </FormField>
  )
}

/** A checkbox with its label on the right; error and helper sit under both. */
export function CheckboxField<T extends FieldValues>({
  name,
  label,
  helper,
  required,
}: BoundProps<T>) {
  const { control, formState } = useFormContext<T>()
  const error = fieldError(formState.errors, name)
  return (
    <Controller
      control={control}
      name={name}
      render={({ field }) => (
        <InlineField
          layout="checkbox"
          label={label}
          helper={helper}
          required={required}
          error={error}
        >
          {(c) => (
            <Checkbox
              id={c.id}
              aria-describedby={c['aria-describedby']}
              aria-invalid={c['aria-invalid']}
              aria-required={c['aria-required']}
              checked={field.value === true}
              onCheckedChange={(checked) => field.onChange(checked === true)}
              onBlur={field.onBlur}
              ref={field.ref}
            />
          )}
        </InlineField>
      )}
    />
  )
}

/** A switch whose label is on the same row; the label names the setting, not the action. */
export function SwitchField<T extends FieldValues>({
  name,
  label,
  helper,
  required,
}: BoundProps<T>) {
  const { control, formState } = useFormContext<T>()
  const error = fieldError(formState.errors, name)
  return (
    <Controller
      control={control}
      name={name}
      render={({ field }) => (
        <InlineField
          layout="switch"
          label={label}
          helper={helper}
          required={required}
          error={error}
        >
          {(c) => (
            <Switch
              id={c.id}
              aria-describedby={c['aria-describedby']}
              aria-invalid={c['aria-invalid']}
              checked={field.value === true}
              onCheckedChange={field.onChange}
              onBlur={field.onBlur}
              ref={field.ref}
            />
          )}
        </InlineField>
      )}
    />
  )
}
