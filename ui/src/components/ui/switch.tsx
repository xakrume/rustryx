import * as SwitchPrimitive from '@radix-ui/react-switch'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

export function Switch({ className, ...props }: ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        'peer inline-flex h-6 w-10 shrink-0 items-center rounded-full border border-transparent bg-input transition-colors duration-200 disabled:cursor-not-allowed disabled:opacity-40 data-[state=checked]:bg-accent',
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="pointer-events-none block size-5 translate-x-px rounded-full bg-on-accent shadow-md transition-transform duration-200 ease-standard data-[state=checked]:translate-x-[1.0625rem]" />
    </SwitchPrimitive.Root>
  )
}
