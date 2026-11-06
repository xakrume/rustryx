import { Slot } from '@radix-ui/react-slot'
import { cva, type VariantProps } from 'class-variance-authority'
import { Loader2 } from 'lucide-react'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/**
 * Fully rounded. Primary actions use the accent token. Hover only changes brightness
 * or a quiet fill: a transform on hover shifts layout under the pointer.
 * Height comes from --control-h, so density and the 44px touch minimum apply everywhere.
 */
export const buttonVariants = cva(
  'inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-full text-sm font-medium tracking-[-0.01em] transition-[background-color,filter,border-color,color] duration-200 ease-standard active:brightness-95 disabled:pointer-events-none disabled:opacity-40 aria-busy:opacity-100! [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        default: 'bg-accent text-on-accent hover:brightness-110',
        secondary: 'bg-secondary text-on-secondary hover:brightness-95 dark:hover:brightness-125',
        outline:
          'border border-input bg-card text-foreground hover:bg-foreground/[0.04] dark:hover:bg-foreground/[0.08]',
        ghost: 'text-foreground hover:bg-foreground/[0.06] dark:hover:bg-foreground/10',
        destructive: 'bg-destructive text-on-destructive hover:brightness-110',
        link: 'rounded-sm px-0 text-link underline-offset-4 hover:underline active:brightness-100',
      },
      size: {
        default: 'h-(--control-h) px-4',
        sm: 'h-[calc(var(--control-h)-0.375rem)] px-3 text-[0.8125rem]',
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
