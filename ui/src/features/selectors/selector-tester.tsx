import { useMutation } from '@tanstack/react-query'
import { Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { REPO_FORMATS } from '@/features/privileges/api'
import { t, type MessageKey } from '@/i18n'
import { testExpression, type CompileIssue, type SelectorSample, type TestResult } from './api'

const MAX_SAMPLES = 50

interface SampleDraft {
  key: number
  format: string
  path: string
  repository: string
  group: string
  name: string
  version: string
}

const first: SampleDraft = {
  key: 1,
  format: 'maven2',
  path: '/com/example/lib/1.0/lib-1.0.jar',
  repository: 'maven-releases',
  group: 'com.example',
  name: 'lib',
  version: '1.0',
}

/** An empty coordinate part is sent as null, which the engine treats as unknown, not as "". */
function toSample(draft: SampleDraft): SelectorSample {
  const part = (value: string): string | null => (value === '' ? null : value)
  return {
    format: draft.format as SelectorSample['format'],
    path: draft.path,
    repository: draft.repository,
    coordinate: {
      group: part(draft.group),
      name: part(draft.name),
      version: part(draft.version),
    },
  }
}

const VERDICT: Record<'match' | 'no_match' | 'unknown', { status: StatusKind; label: MessageKey }> =
  {
    match: { status: 'ok', label: 'selectors.tester.match' },
    no_match: { status: 'neutral', label: 'selectors.tester.noMatch' },
    unknown: { status: 'warning', label: 'selectors.tester.unknown' },
  }

export interface SelectorTesterProps {
  expression: string
  onIssues: (issues: readonly CompileIssue[]) => void
}

/**
 * Evaluates the expression against sample format, path, repository and coordinate input
 * (`POST /content-selectors/test`, at most 50 samples). The verdict per sample is text with an
 * icon: match, no match, or unknown with the reason (a coordinate part the sample does not have).
 */
export function SelectorTester({ expression, onIssues }: SelectorTesterProps) {
  const [samples, setSamples] = useState<SampleDraft[]>([first])
  const [nextKey, setNextKey] = useState(2)
  const run = useMutation({
    mutationFn: (): Promise<TestResult> => testExpression(expression, samples.map(toSample)),
    onSuccess: (result) => {
      if (!result.valid) onIssues(result.errors)
    },
  })

  function patch(key: number, change: Partial<SampleDraft>): void {
    setSamples((current) => current.map((s) => (s.key === key ? { ...s, ...change } : s)))
  }

  const result = run.data
  const matches = result?.results.filter((r) => r.result === 'match').length ?? 0

  return (
    <section aria-labelledby="tester-title" className="space-y-3">
      <div>
        <h3 id="tester-title" className="text-sm font-semibold">
          {t('selectors.tester.title')}
        </h3>
        <p className="text-xs text-muted-foreground">{t('selectors.tester.description')}</p>
      </div>
      <ol className="space-y-3">
        {samples.map((sample, index) => {
          const verdict = result?.valid ? result.results.find((r) => r.index === index) : undefined
          const id = `sample-${sample.key}`
          const field = (name: keyof SampleDraft, label: string, mono = true, helper?: string) => (
            <div className="space-y-1">
              <Label htmlFor={`${id}-${name}`} className="text-xs">
                {label}
              </Label>
              <Input
                id={`${id}-${name}`}
                value={String(sample[name])}
                className={mono ? 'font-mono' : undefined}
                spellCheck={false}
                autoComplete="off"
                aria-describedby={helper ? `${id}-${name}-helper` : undefined}
                onChange={(e) => patch(sample.key, { [name]: e.target.value })}
              />
              {helper ? (
                <p id={`${id}-${name}-helper`} className="text-xs text-muted-foreground">
                  {helper}
                </p>
              ) : null}
            </div>
          )
          return (
            <li key={sample.key} className="rounded-md border border-border p-3">
              <div className="mb-2 flex items-center justify-between gap-2">
                <p className="text-sm font-medium">
                  {t('selectors.tester.sample', { number: index + 1 })}
                </p>
                {samples.length > 1 ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t('selectors.tester.remove', { number: index + 1 })}
                    onClick={() => setSamples((c) => c.filter((s) => s.key !== sample.key))}
                  >
                    <Trash2 aria-hidden="true" />
                  </Button>
                ) : null}
              </div>
              <div className="grid gap-2 sm:grid-cols-2">
                <div className="space-y-1">
                  <Label htmlFor={`${id}-format`} className="text-xs">
                    {t('selectors.tester.format')}
                  </Label>
                  <Select
                    id={`${id}-format`}
                    value={sample.format}
                    onChange={(e) => patch(sample.key, { format: e.target.value })}
                  >
                    {REPO_FORMATS.map((f) => (
                      <option key={f} value={f}>
                        {f}
                      </option>
                    ))}
                  </Select>
                </div>
                {field('repository', t('selectors.tester.repository'))}
                <div className="sm:col-span-2">
                  {field(
                    'path',
                    t('selectors.tester.path'),
                    true,
                    t('selectors.tester.pathHelper'),
                  )}
                </div>
                {field('group', t('selectors.tester.group'))}
                {field('name', t('selectors.tester.name'))}
                {field('version', t('selectors.tester.version'))}
              </div>
              {verdict ? (
                <p
                  className="mt-2 flex flex-wrap items-center gap-2 text-sm"
                  aria-label={t('selectors.tester.resultFor', { number: index + 1 })}
                >
                  <StatusBadge
                    status={VERDICT[verdict.result].status}
                    label={t(VERDICT[verdict.result].label)}
                  />
                  {verdict.reason ? (
                    <span className="text-xs text-muted-foreground">{verdict.reason}</span>
                  ) : null}
                </p>
              ) : null}
            </li>
          )
        })}
      </ol>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          disabled={samples.length >= MAX_SAMPLES}
          onClick={() => {
            setSamples((c) => [...c, { ...first, key: nextKey }])
            setNextKey((k) => k + 1)
          }}
        >
          <Plus aria-hidden="true" />
          {t('selectors.tester.add')}
        </Button>
        <Button
          loading={run.isPending}
          disabled={expression.trim() === ''}
          onClick={() => run.mutate()}
        >
          {run.isPending ? t('selectors.tester.running') : t('selectors.tester.run')}
        </Button>
        {expression.trim() === '' ? (
          <span className="text-xs text-muted-foreground">
            {t('selectors.tester.needExpression')}
          </span>
        ) : null}
      </div>
      <div role="status" className="text-sm">
        {result?.valid === true ? (
          <p className="font-medium">
            {t('selectors.tester.summary', { matches, count: result.results.length })}
          </p>
        ) : null}
        {result?.valid === false ? (
          <p className="text-destructive-text">{t('selectors.tester.invalidExpression')}</p>
        ) : null}
      </div>
    </section>
  )
}
