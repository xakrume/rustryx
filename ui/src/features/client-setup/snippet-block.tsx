import { Eye, EyeOff } from 'lucide-react'
import { useId, useState } from 'react'
import { CopyButton } from '@/components/copy-button'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import type { ClientSnippet } from './api'

const MASK = '•'.repeat(24)

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** Replace `<NAME>` markers with real values, for the current page view only. */
export function fillPlaceholders(
  content: string,
  values: Readonly<Record<string, string>>,
): string {
  return Object.entries(values).reduce(
    (text, [name, value]) => text.split(`<${name}>`).join(value),
    content,
  )
}

export interface SnippetBlockProps {
  snippet: ClientSnippet
  /** The text to show and copy, after any placeholder was filled. */
  content: string
  /** True when `content` holds a real token: hidden until revealed, and out of the DOM while hidden. */
  secret: boolean
  /** Show the exact text with no markers (what a copy yields). */
  raw: boolean
}

function Highlighted({ content, names }: { content: string; names: readonly string[] }) {
  if (names.length === 0) return <>{content}</>
  const pattern = new RegExp(`(${names.map((n) => escapeRegExp(`<${n}>`)).join('|')})`)
  return (
    <>
      {content.split(pattern).map((part, index) =>
        // The even indexes of a split with a capture group are the text between markers.
        index % 2 === 1 ? (
          <span
            key={index}
            data-placeholder=""
            className="rounded-sm bg-warning-subtle px-0.5 font-medium text-warning-text underline decoration-dotted underline-offset-2"
          >
            {part}
          </span>
        ) : (
          part
        ),
      )}
    </>
  )
}

/**
 * One configuration snippet: title, what it is for, the file it belongs to, the text with its
 * placeholders marked (underlined as well as tinted, so the cue is not color alone), and a copy
 * button. Copying always yields the exact text. A snippet that carries a real token keeps it out
 * of the DOM until the user reveals it, as the shared code block does.
 */
export function SnippetBlock({ snippet, content, secret, raw }: SnippetBlockProps) {
  const [revealed, setRevealed] = useState(false)
  const labelId = useId()
  const masked = secret && !revealed
  const what = snippet.title
  return (
    <figure className="overflow-hidden rounded-lg border border-border bg-muted">
      <figcaption
        id={labelId}
        className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 border-b border-border px-3 py-1.5 text-xs"
      >
        <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span className="font-medium text-foreground">{snippet.title}</span>
          <Badge variant="neutral">{t(`clientSetup.purpose.${snippet.purpose}`)}</Badge>
          {snippet.file ? (
            <span className="font-mono text-muted-foreground">
              <span className="sr-only">{t('clientSetup.file')}: </span>
              {snippet.file}
            </span>
          ) : null}
        </span>
        <span className="flex items-center gap-1">
          {secret ? (
            <Button
              variant="ghost"
              size="icon"

              aria-pressed={revealed}
              aria-label={revealed ? t('code.hide') : t('code.reveal')}
              onClick={() => setRevealed((r) => !r)}
            >
              {revealed ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
            </Button>
          ) : null}
          <CopyButton value={content} what={what} size="icon" />
        </span>
      </figcaption>
      <pre
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll long lines
        tabIndex={0}
        aria-labelledby={labelId}
        className={cn(
          'overflow-x-auto p-3 font-mono text-[0.8125rem] leading-5 text-foreground',
          masked && 'select-none',
        )}
      >
        {masked ? (
          <code aria-label={t('code.hidden')}>{MASK}</code>
        ) : (
          <code data-language={snippet.language}>
            {raw || secret ? (
              content
            ) : (
              <Highlighted content={content} names={snippet.placeholders.map((p) => p.name)} />
            )}
          </code>
        )}
      </pre>
    </figure>
  )
}
