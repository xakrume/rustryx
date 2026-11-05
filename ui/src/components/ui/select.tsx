import { ChevronDown } from 'lucide-react'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'
import { fieldClasses } from './input'

/**
 * A styled native <select>. Radix Select was not taken on purpose: its content runs
 * react-remove-scroll, which injects a <style> element that the strict CSP blocks, and the
 * native control is faster, fully accessible and the right choice on touch devices.
 */
export function Select({ className, children, ...props }: ComponentProps<'select'>) {
  return (
    <span className="relative inline-flex w-full items-center">
      <select
        className={cn(fieldClasses, 'h-(--control-h) appearance-none pr-8', className)}
        {...props}
      >
        {children}
      </select>
      <ChevronDown
        className="pointer-events-none absolute right-2.5 size-4 text-muted-foreground"
        aria-hidden="true"
      />
    </span>
  )
}
