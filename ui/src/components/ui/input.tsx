import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/** Control boundary is --color-input (3:1 against the surface), not the decorative border. */
export const fieldClasses =
  'w-full rounded-md border border-input bg-card px-3 text-sm text-foreground placeholder:text-muted-foreground transition-[border-color,box-shadow] duration-200 hover:border-muted-foreground focus-visible:border-accent focus-visible:outline-offset-0 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:border-2 read-only:bg-muted'

export function Input({ className, type = 'text', ...props }: ComponentProps<'input'>) {
  return <input type={type} className={cn(fieldClasses, 'h-(--control-h)', className)} {...props} />
}

export function Textarea({ className, ...props }: ComponentProps<'textarea'>) {
  return <textarea className={cn(fieldClasses, 'min-h-20 py-2', className)} {...props} />
}
