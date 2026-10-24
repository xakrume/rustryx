import { CircleAlert, CircleCheck, Loader2 } from 'lucide-react'
import { useId, useRef, type Ref } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/input'
import { t } from '@/i18n'
import { type CompileIssue } from './api'
import { byteLength, locate } from './positions'
import type { LiveValidation } from './use-live-validation'

export interface ExpressionFieldProps {
  value: string
  onChange: (next: string) => void
  onBlur?: () => void
  validation: LiveValidation
  /** Problems reported by a save attempt; shown instead of the live ones until the text changes. */
  issues: readonly CompileIssue[] | null
  /** The message of the form for an empty expression. */
  error?: string
  limit: number
  textareaRef?: Ref<HTMLTextAreaElement>
}

/**
 * The expression box with live feedback: the verdict of the server (valid, or each problem with
 * its kind, message, line and column), a code frame marking the position, a "go to" button that
 * puts the caret there, and the byte counter against the 1 KiB limit. Positions are bytes in the
 * API and converted to text positions here. The verdict is a polite status; the list of problems
 * is plain content, so typing is never interrupted by announcements.
 */
export function ExpressionField({
  value,
  onChange,
  onBlur,
  validation,
  issues,
  error,
  limit,
  textareaRef,
}: ExpressionFieldProps) {
  const base = useId()
  const innerRef = useRef<HTMLTextAreaElement | null>(null)
  const shown = issues ?? validation.errors
  const bytes = byteLength(value)
  const invalid = shown.length > 0 || error !== undefined

  function goTo(index: number): void {
    const box = innerRef.current
    if (box === null) return
    box.focus()
    box.setSelectionRange(index, index)
  }

  return (
    <div className="space-y-1.5">
      <Label htmlFor={`${base}-box`}>
        {t('selectors.editor.expression')}
        <span aria-hidden="true" className="ml-0.5 text-destructive-text">
          *
        </span>
        <span className="sr-only"> ({t('form.required')})</span>
      </Label>
      <Textarea
        id={`${base}-box`}
        ref={(node) => {
          innerRef.current = node
          if (typeof textareaRef === 'function') textareaRef(node)
          else if (textareaRef) textareaRef.current = node
        }}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onBlur}
        rows={5}
        spellCheck={false}
        autoComplete="off"
        autoCapitalize="none"
        aria-required="true"
        aria-invalid={invalid ? true : undefined}
        aria-describedby={`${base}-help ${base}-status`}
        className="font-mono text-[0.8125rem] leading-5"
      />
      <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
        <p id={`${base}-help`}>{t('selectors.editor.expressionHelper')}</p>
        <p className={bytes > limit ? 'font-medium text-destructive-text' : undefined}>
          {t('selectors.editor.bytes', { used: bytes, limit })}
        </p>
      </div>
      <div id={`${base}-status`}>
        <p role="status" className="flex items-center gap-1.5 text-sm">
          {error !== undefined && shown.length === 0 ? (
            <>
              <CircleAlert className="size-4 text-destructive-text" aria-hidden="true" />
              <span className="font-medium text-destructive-text">{error}</span>
            </>
          ) : shown.length > 0 ? (
            <>
              <CircleAlert className="size-4 text-destructive-text" aria-hidden="true" />
              <span className="font-medium text-destructive-text">
                {t('selectors.validation.invalid', { count: shown.length })}
              </span>
            </>
          ) : validation.status === 'checking' ? (
            <>
              <Loader2 className="size-4 animate-spin-slow" aria-hidden="true" />
              <span className="text-muted-foreground">{t('selectors.validation.checking')}</span>
            </>
          ) : validation.status === 'valid' ? (
            <>
              <CircleCheck className="size-4 text-success-text" aria-hidden="true" />
              <span className="text-success-text">{t('selectors.validation.valid')}</span>
            </>
          ) : validation.status === 'failed' ? (
            <>
              <CircleAlert className="size-4 text-warning-text" aria-hidden="true" />
              <span className="text-warning-text">{t('selectors.validation.failed')}</span>
            </>
          ) : null}
        </p>
        {shown.length > 0 ? (
          <ul aria-label={t('selectors.validation.listLabel')} className="mt-2 space-y-2">
            {shown.map((issue, i) => {
              const where = locate(value, issue.offset)
              return (
                <li
                  key={`${issue.kind}-${issue.offset}-${i}`}
                  className="rounded-md border border-destructive bg-destructive-subtle p-2 text-sm text-destructive-text"
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="destructive">
                      {t(`selectors.validation.kinds.${issue.kind}`)}
                    </Badge>
                    <span>{issue.message}</span>
                  </div>
                  {where ? (
                    <div className="mt-1 flex flex-wrap items-center gap-2">
                      <span className="text-xs">
                        {t('selectors.validation.position', {
                          line: where.line,
                          column: where.column,
                        })}
                      </span>
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={`${t('selectors.validation.goTo')}: ${t('selectors.validation.position', { line: where.line, column: where.column })}`}
                        onClick={() => goTo(where.index)}
                      >
                        {t('selectors.validation.goTo')}
                      </Button>
                    </div>
                  ) : (
                    <p className="mt-1 text-xs">{t('selectors.validation.noPosition')}</p>
                  )}
                  {where ? (
                    <pre
                      aria-hidden="true"
                      className="mt-1 overflow-x-auto font-mono text-xs leading-4"
                    >
                      {`${where.lineText}\n${' '.repeat(where.column - 1)}^`}
                    </pre>
                  ) : null}
                </li>
              )
            })}
          </ul>
        ) : null}
      </div>
    </div>
  )
}
