import { cva, type VariantProps } from 'class-variance-authority'
import { AlertTriangle, CheckCircle2, Info, XCircle } from 'lucide-react'
import type { ComponentProps, ReactNode } from 'react'
import { cn } from '@/lib/utils'

const alertVariants = cva('flex gap-2 rounded-md border px-3 py-2 text-sm', {
  variants: {
    variant: {
      info: 'border-info bg-info-subtle text-info-text',
      success: 'border-success bg-success-subtle text-success-text',
      warning: 'border-warning bg-warning-subtle text-warning-text',
      destructive: 'border-destructive bg-destructive-subtle text-destructive-text',
    },
  },
  defaultVariants: { variant: 'info' },
})

const icons = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  destructive: XCircle,
} as const

export interface AlertProps
  extends Omit<ComponentProps<'div'>, 'title'>, VariantProps<typeof alertVariants> {
  title?: ReactNode
}

/**
 * Inline message. The icon plus the text carry the meaning, never the color alone. Errors use
 * role="alert" (announced at once), everything else role="status" (announced politely).
 */
export function Alert({ className, variant = 'info', title, children, ...props }: AlertProps) {
  const Icon = icons[variant ?? 'info']
  return (
    <div
      role={variant === 'destructive' ? 'alert' : 'status'}
      className={cn(alertVariants({ variant }), className)}
      {...props}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div className="min-w-0 flex-1">
        {title ? <p className="font-medium">{title}</p> : null}
        {children ? <div className={title ? 'mt-0.5' : undefined}>{children}</div> : null}
      </div>
    </div>
  )
}
