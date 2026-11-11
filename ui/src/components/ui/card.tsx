import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

export function Card({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      className={cn(
        'rounded-xl border border-border/50 bg-card text-card-foreground shadow-sm dark:border-border/60 dark:shadow-none',
        className,
      )}
      {...props}
    />
  )
}

export function CardHeader({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      className={cn('flex flex-col gap-1 px-5 pt-5 pb-2 md:px-6 md:pt-6', className)}
      {...props}
    />
  )
}

export function CardTitle({ className, ...props }: ComponentProps<'h2'>) {
  const classes = cn('text-[1.0625rem] leading-6 font-semibold tracking-[-0.015em]', className)
  // eslint-disable-next-line jsx-a11y/heading-has-content -- the content is the children in props
  return <h2 className={classes} {...props} />
}

export function CardDescription({ className, ...props }: ComponentProps<'p'>) {
  return <p className={cn('text-sm text-muted-foreground', className)} {...props} />
}

export function CardContent({ className, ...props }: ComponentProps<'div'>) {
  return <div className={cn('px-5 pt-2 pb-5 md:px-6 md:pb-6', className)} {...props} />
}

export function CardFooter({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      className={cn('flex items-center gap-2 px-5 pb-5 md:px-6 md:pb-6', className)}
      {...props}
    />
  )
}
