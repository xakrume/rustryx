import * as ToastPrimitive from '@radix-ui/react-toast'
import { X } from 'lucide-react'
import type { ComponentProps } from 'react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

/**
 * Toasts are Radix Toast, not sonner. sonner injects its stylesheet as a <style> element at
 * runtime, which the strict CSP (`style-src 'self'`) blocks. Radix Toast keeps the ARIA live
 * region, the swipe gesture, pause on hover and focus, and the F8 hotkey to reach the stack.
 */
export const ToastProvider = ToastPrimitive.Provider

export function ToastViewport({
  className,
  ...props
}: ComponentProps<typeof ToastPrimitive.Viewport>) {
  return (
    <ToastPrimitive.Viewport
      label={t('toast.notifications')}
      className={cn(
        'fixed right-0 bottom-0 z-[100] m-0 flex max-h-dvh w-full max-w-md list-none flex-col gap-2 p-4 outline-none',
        className,
      )}
      {...props}
    />
  )
}

export function ToastRoot({ className, ...props }: ComponentProps<typeof ToastPrimitive.Root>) {
  return (
    <ToastPrimitive.Root
      className={cn(
        'relative flex items-start gap-3 rounded-lg border border-border border-l-4 bg-popover p-3 pr-10 text-sm text-popover-foreground shadow-lg data-[state=closed]:animate-toast-out data-[state=open]:animate-toast-in',
        className,
      )}
      {...props}
    />
  )
}

export const ToastTitle = ToastPrimitive.Title
export const ToastDescription = ToastPrimitive.Description
export const ToastAction = ToastPrimitive.Action

export function ToastClose({ className, ...props }: ComponentProps<typeof ToastPrimitive.Close>) {
  return (
    <ToastPrimitive.Close
      aria-label={t('toast.dismiss')}
      className={cn(
        'absolute top-1.5 right-1.5 inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground',
        className,
      )}
      {...props}
    >
      <X className="size-4" aria-hidden="true" />
    </ToastPrimitive.Close>
  )
}
