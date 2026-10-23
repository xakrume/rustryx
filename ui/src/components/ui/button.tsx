import { Slot } from '@radix-ui/react-slot'
import { cva, type VariantProps } from 'class-variance-authority'
import { Loader2 } from 'lucide-react'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/**
 * Primary actions use the accent (green) token, as the design brief says. Hover changes opacity
 * only: a transform on hover shifts layout under the pointer (MASTER.md anti-pattern).
 * Height comes from --control-h, so density and the 44px touch minimum apply everywhere.
 */
export const buttonVariants = cva(
  'inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-[background-color,opacity,border-color] duration-150 hover:opacity-90 disabled:pointer-events-none disabled:opacity-50 aria-busy:opacity-100! [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        default: 'bg-accent text-on-accent',
        secondary: 'bg-secondary text-on-secondary',
        outline: 'border border-input bg-card text-foreground hover:bg-muted hover:opacity-100',
        ghost: 'text-foreground hover:bg-muted hover:opacity-100',
        destructive: 'bg-destructive text-on-destructive',
        link: 'text-link underline-offset-4 hover:underline hover:opacity-100',
      },
      size: {
        default: 'h-(--control-h) px-3',
        sm: 'h-[calc(var(--control-h)-0.25rem)] px-2.5 text-[0.8125rem]',
        icon: 'size-(--control-h)',
      },
    },
    defaultVariants: { variant: 'default', size: 'default' },
  },
)

export interface ButtonProps extends ComponentProps<'button'>, VariantProps<typeof buttonVariants> {
  asChild?: boolean
  /** Shows a spinner and blocks clicks. The label stays so the button does not change width. */
  loading?: boolean
}

export function Button({
  className,
  variant,
  size,
  asChild = false,
  loading = false,
  disabled,
  children,
  type = 'button',
  ...props
}: ButtonProps) {
  const classes = cn(buttonVariants({ variant, size }), className)
  if (asChild) {
    return (
      <Slot className={classes} {...props}>
        {children}
      </Slot>
    )
  }
  return (
    <button
      type={type}
      className={classes}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...props}
    >
      {loading ? <Loader2 className="animate-spin-slow" aria-hidden="true" /> : null}
      {children}
    </button>
  )
}
