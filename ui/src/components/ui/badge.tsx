import { cva, type VariantProps } from 'class-variance-authority'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/** Tint plus text: the *-text token is checked at 4.5:1 on its *-subtle background. */
export const badgeVariants = cva(
  'inline-flex items-center gap-1 rounded-full border border-transparent px-2.5 py-0.5 text-xs font-medium whitespace-nowrap [&_svg]:size-3.5 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        neutral: 'bg-secondary text-on-secondary',
        success: 'bg-success-subtle text-success-text',
        warning: 'bg-warning-subtle text-warning-text',
        destructive: 'bg-destructive-subtle text-destructive-text',
        info: 'bg-info-subtle text-info-text',
      },
    },
    defaultVariants: { variant: 'neutral' },
  },
)

export function Badge({
  className,
  variant,
  ...props
}: ComponentProps<'span'> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />
}
