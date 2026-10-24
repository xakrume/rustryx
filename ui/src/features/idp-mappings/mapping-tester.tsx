import { useMutation } from '@tanstack/react-query'
import { CircleAlert, CircleCheck } from 'lucide-react'
import { useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/input'
import { t } from '@/i18n'
import { testMappings, type Provider } from './api'

const MAX_GROUPS = 256

/** One group per line; a line is sent exactly as typed, only completely empty lines are dropped. */
export function parseGroups(text: string): string[] {
  return text.split('\n').filter((line) => line !== '')
}

/**
 * The tester of the rule table: enter the groups of a user and see the roles they would receive
 * (`POST /idp-group-mappings/test`), each with the reason (admin group, mapping, default role), the
 * groups that grant nothing, and whether the sign in would even be accepted.
 */
export function MappingTester({ provider }: { provider: Provider }) {
  const [text, setText] = useState('')
  const [error, setError] = useState<string | undefined>()
  const run = useMutation({ mutationFn: (groups: string[]) => testMappings(provider, groups) })

  function submit(): void {
    const groups = parseGroups(text)
    if (groups.length === 0 || groups.length > MAX_GROUPS) {
      setError(t('mappings.tester.groupsRequired'))
      return
    }
    setError(undefined)
    run.mutate(groups)
  }

  const result = run.data
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('mappings.tester.title')}</CardTitle>
        <CardDescription>{t('mappings.tester.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="space-y-1.5">
          <Label htmlFor={`tester-groups-${provider}`}>{t('mappings.tester.groups')}</Label>
          <Textarea
            id={`tester-groups-${provider}`}
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={4}
            spellCheck={false}
            autoComplete="off"
            autoCapitalize="none"
            aria-invalid={error ? true : undefined}
            aria-describedby={`tester-groups-${provider}-help`}
            className="font-mono text-[0.8125rem]"
          />
          <p id={`tester-groups-${provider}-help`} className="text-xs text-muted-foreground">
            {error ? (
              <span className="font-medium text-destructive-text">{error}</span>
            ) : (
              t('mappings.tester.groupsHelper')
            )}
          </p>
        </div>
        <Button variant="outline" loading={run.isPending} onClick={submit}>
          {run.isPending ? t('mappings.tester.running') : t('mappings.tester.run')}
        </Button>
        <div role="status" className="space-y-3 text-sm">
          {result ? (
            <>
              <h3 className="font-medium">{t('mappings.tester.resultTitle')}</h3>
              {result.roles.length === 0 ? (
                <p className="text-muted-foreground">{t('mappings.tester.none')}</p>
              ) : (
                <ul className="divide-y divide-border">
                  {result.roles.map((entry, i) => (
                    <li
                      key={`${entry.role.id}-${entry.via}-${entry.group ?? ''}-${i}`}
                      className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5"
                    >
                      <span className="font-mono font-medium">{entry.role.name}</span>
                      <Badge variant="info">{t(`mappings.tester.via.${entry.via}`)}</Badge>
                      {entry.group ? (
                        <span className="font-mono text-xs break-all text-muted-foreground">
                          {t('mappings.tester.from', { group: entry.group })}
                        </span>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
              {result.unmatched_groups.length > 0 ? (
                <div className="space-y-1">
                  <p className="font-medium">{t('mappings.tester.unmatched')}</p>
                  <ul className="space-y-0.5 font-mono text-xs break-all text-muted-foreground">
                    {result.unmatched_groups.map((group) => (
                      <li key={group}>{group}</li>
                    ))}
                  </ul>
                </div>
              ) : null}
              <p className="flex items-center gap-2 font-medium">
                {result.denied ? (
                  <>
                    <CircleAlert className="size-4 text-destructive-text" aria-hidden="true" />
                    <span className="text-destructive-text">{t('mappings.tester.denied')}</span>
                  </>
                ) : (
                  <>
                    <CircleCheck className="size-4 text-success-text" aria-hidden="true" />
                    <span className="text-success-text">{t('mappings.tester.allowed')}</span>
                  </>
                )}
              </p>
            </>
          ) : null}
        </div>
      </CardContent>
    </Card>
  )
}
