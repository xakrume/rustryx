import { Eye, EyeOff } from 'lucide-react'
import { useId, useState } from 'react'
import { CopyButton } from '@/components/copy-button'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

export interface CodeBlockProps {
  /** The text shown and copied. */
  value: string
  /** Visible caption and accessible name: "Maven settings.xml", "Pull command". */
  label: string
  /**
   * A secret (token, password): hidden by default, and while hidden its text is NOT in the
   * DOM, so select-all, find in page and screen reader browsing cannot expose it. The copy
   * button still copies the real value.
   */
  secret?: boolean
  /** Wrap long lines instead of scrolling sideways. Off for commands, on for prose-like output. */
  wrap?: boolean
  className?: string
}

const MASK = '•'.repeat(24)

/**
 * Technical values (commands, config, digests, paths) in Fira Code with one-click copy.
 * The block is a focusable region so keyboard users can scroll long lines.
 */
export function CodeBlock({
  value,
  label,
  secret = false,
  wrap = false,
  className,
}: CodeBlockProps) {
  const [revealed, setRevealed] = useState(false)
  const labelId = useId()
  const masked = secret && !revealed

  return (
    <figure className={cn('overflow-hidden rounded-lg border border-border bg-muted', className)}>
      <figcaption
        id={labelId}
        className="flex items-center justify-between gap-2 border-b border-border px-3 py-1 text-xs font-medium text-muted-foreground"
      >
        <span>{label}</span>
        <span className="flex items-center gap-1">
          {secret ? (
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              aria-pressed={revealed}
              aria-label={revealed ? t('code.hide') : t('code.reveal')}
              onClick={() => setRevealed((r) => !r)}
            >
              {revealed ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
            </Button>
          ) : null}
          <CopyButton value={value} what={label} size="icon" className="size-7" />
        </span>
      </figcaption>
      <pre
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll long lines
        tabIndex={0}
        aria-labelledby={labelId}
        className={cn(
          'p-3 font-mono text-[0.8125rem] leading-5 text-foreground',
          wrap ? 'break-all whitespace-pre-wrap' : 'overflow-x-auto',
          masked && 'select-none',
        )}
      >
        {masked ? <code aria-label={t('code.hidden')}>{MASK}</code> : <code>{value}</code>}
      </pre>
    </figure>
  )
}
