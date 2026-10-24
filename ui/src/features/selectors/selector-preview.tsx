import { useQuery } from '@tanstack/react-query'
import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { repositoryChoicesQuery } from '@/features/privileges/api'
import { t } from '@/i18n'
import { previewExpression, type CompileIssue } from './api'

export function SelectorPreview({
  expression,
  onIssues,
}: {
  expression: string
  onIssues: (issues: readonly CompileIssue[]) => void
}) {
  const repositories = useQuery(repositoryChoicesQuery)
  const [repository, setRepository] = useState('')
  const preview = useMutation({
    mutationFn: () => previewExpression(expression, repository),
    onSuccess: (result) => {
      if (!result.valid) onIssues(result.errors)
    },
  })
  const result = preview.data

  return (
    <section aria-labelledby="preview-title" className="space-y-3">
      <div>
        <h3 id="preview-title" className="text-sm font-semibold">
          {t('selectors.preview.title')}
        </h3>
        <p className="text-xs text-muted-foreground">{t('selectors.preview.description')}</p>
      </div>
      <div className="flex flex-wrap items-end gap-2">
        <div className="min-w-48 flex-1 space-y-1">
          <Label htmlFor="preview-repository" className="text-xs">
            {t('selectors.preview.repository')}
          </Label>
          <Select
            id="preview-repository"
            value={repository}
            onChange={(e) => setRepository(e.target.value)}
          >
            <option value="">{t('selectors.preview.choose')}</option>
            {(repositories.data ?? []).map((r) => (
              <option key={r.name} value={r.name}>
                {r.name}
              </option>
            ))}
          </Select>
        </div>
        <Button
          variant="outline"
          loading={preview.isPending}
          disabled={repository === '' || expression.trim() === ''}
          onClick={() => preview.mutate()}
        >
          {preview.isPending ? t('selectors.preview.running') : t('selectors.preview.run')}
        </Button>
      </div>
      <div role="status" className="text-sm">
        {result?.valid === true ? (
          result.items.length === 0 ? (
            <p className="text-muted-foreground">{t('selectors.preview.none')}</p>
          ) : (
            <>
              <p className="font-medium">
                {t('selectors.preview.hits', { count: result.items.length })}
              </p>
              <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto font-mono text-xs">
                {result.items.map((hit) => (
                  <li key={hit.asset_id} className="break-all">
                    {hit.path}
                  </li>
                ))}
              </ul>
              {result.truncated ? (
                <p className="mt-1 text-xs text-muted-foreground">
                  {t('selectors.preview.truncated')}
                </p>
              ) : null}
            </>
          )
        ) : null}
      </div>
    </section>
  )
}
