import { Check, Copy } from 'lucide-react'
import { useEffect, useRef, useState, type ComponentProps } from 'react'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { Button } from '@/components/ui/button'

/**
 * Copy text to the clipboard. The async Clipboard API needs a secure context (https or
 * localhost); an operator that serves the UI over plain http behind a proxy would otherwise
 * get a dead button, so a textarea and execCommand are the fallback. The temporary textarea
 * holds the value for one tick and is removed, nothing stays in the DOM.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard) {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // Denied or unavailable: fall through to the legacy path.
  }
  try {
    const area = document.createElement('textarea')
    area.value = text
    area.setAttribute('readonly', '')
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.appendChild(area)
    area.select()
    const ok = document.execCommand('copy')
    area.value = ''
    area.remove()
    return ok
  } catch {
    return false
  }
}

export interface CopyButtonProps extends Omit<
  ComponentProps<typeof Button>,
  'onClick' | 'children' | 'value'
> {
  /** The text that is copied. It is never rendered by this component. */
  value: string
  /** What is being copied, for the accessible name and the toast: "digest", "token". */
  what: string
  /** Show a text label next to the icon. Icon only by default. */
  showLabel?: boolean
  /** Skip the toast, for places that announce the copy themselves. */
  silent?: boolean
}

export function CopyButton({
  value,
  what,
  showLabel = false,
  silent = false,
  variant = 'ghost',
  size,
  className,
  ...props
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current)
    },
    [],
  )

  async function onClick(): Promise<void> {
    const ok = await copyText(value)
    if (!ok) {
      toast.error(t('code.copyFailed'))
      return
    }
    setCopied(true)
    if (!silent) toast.success(t('code.copied'), { description: what, durationMs: 2500 })
    if (timer.current !== null) clearTimeout(timer.current)
    timer.current = setTimeout(() => setCopied(false), 2000)
  }

  return (
    <Button
      variant={variant}
      size={size ?? (showLabel ? 'sm' : 'icon')}
      className={className}
      aria-label={showLabel ? undefined : t('code.copy', { what })}
      onClick={() => void onClick()}
      {...props}
    >
      {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      {showLabel ? <span>{copied ? t('common.copied') : t('code.copy', { what })}</span> : null}
    </Button>
  )
}
