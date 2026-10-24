import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useFormContext, useWatch } from 'react-hook-form'
import { ErrorPanel } from '@/components/error-panel'
import { StatusBadge } from '@/components/status-badge'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/input'
import { t } from '@/i18n'
import { testDraftRule, type RoutingTestResult } from './api'
import { usablePatterns, type RuleFormValues } from './rule-model'

const MAX_PATHS = 100

/**
 * "What would this rule do with these paths?" against the rule as it is in the form, saved or
 * not. Each path gets an outcome in words and the pattern that decided it, so a rule is
 * understood before a repository uses it.
 */
export function RuleTester() {
  const { control } = useFormContext<RuleFormValues>()
  const mode = useWatch({ control, name: 'mode' })
  const patterns = usablePatterns({ patterns: useWatch({ control, name: 'patterns' }) })
  const [text, setText] = useState('')
  const [hint, setHint] = useState<string | null>(null)
  const [result, setResult] = useState<RoutingTestResult | null>(null)
  const [failure, setFailure] = useState<unknown>(null)

  const run = useMutation({
    mutationFn: (paths: string[]) => testDraftRule(mode, patterns, paths),
    meta: { silent: true },
    onMutate: () => {
      setFailure(null)
      setHint(null)
    },
    onSuccess: setResult,
    onError: (error) => {
      setResult(null)
      setFailure(error)
    },
  })

  function submit(): void {
    const paths = text
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '')
    if (patterns.length === 0) return setHint(t('routing.test.needPatterns'))
    if (paths.length === 0) return setHint(t('routing.test.needPaths'))
    if (paths.length > MAX_PATHS) return setHint(t('routing.test.tooMany', { max: MAX_PATHS }))
    run.mutate(paths)
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('routing.test.title')}</CardTitle>
        <CardDescription>{t('routing.test.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="space-y-1.5">
          <Label htmlFor="test-paths">{t('routing.test.paths')}</Label>
          <Textarea
            id="test-paths"
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={4}
            className="font-mono"
            spellCheck={false}
            aria-describedby="test-paths-helper"
          />
          <p id="test-paths-helper" className="text-xs text-muted-foreground">
            {t('routing.test.pathsHelper')}
          </p>
        </div>
        <Button variant="outline" loading={run.isPending} onClick={submit}>
          {run.isPending ? t('routing.test.running') : t('routing.test.run')}
        </Button>
        {hint ? <Alert variant="warning">{hint}</Alert> : null}
        {failure !== null ? <ErrorPanel error={failure} /> : null}
        {result ? (
          <div
            className="overflow-x-auto"
            role="region"
            aria-label={t('routing.test.resultsLabel')}
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll the table
            tabIndex={0}
          >
            <table className="w-full text-left text-(length:--text-table)">
              <thead>
                <tr className="border-b border-border text-muted-foreground">
                  <th scope="col" className="py-1 pr-3 font-medium">
                    {t('routing.test.columns.path')}
                  </th>
                  <th scope="col" className="py-1 pr-3 font-medium">
                    {t('routing.test.columns.outcome')}
                  </th>
                  <th scope="col" className="py-1 pr-3 font-medium">
                    {t('routing.test.columns.pattern')}
                  </th>
                  <th scope="col" className="py-1 font-medium">
                    {t('routing.test.columns.reason')}
                  </th>
                </tr>
              </thead>
              <tbody>
                {result.items.map((item) => (
                  <tr key={item.path} className="border-b border-border last:border-0">
                    <th scope="row" className="py-1 pr-3 font-mono font-normal break-all">
                      {item.path}
                    </th>
                    <td className="py-1 pr-3 whitespace-nowrap">
                      {item.outcome === 'allowed' ? (
                        <StatusBadge status="ok" label={t('routing.test.allowed')} />
                      ) : (
                        <StatusBadge status="error" label={t('routing.test.blocked')} />
                      )}
                    </td>
                    <td className="py-1 pr-3 font-mono text-xs break-all">
                      {item.matched_pattern ?? (
                        <span className="text-muted-foreground">{t('routing.test.noPattern')}</span>
                      )}
                    </td>
                    <td className="py-1">{item.reason}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
      </CardContent>
    </Card>
  )
}
