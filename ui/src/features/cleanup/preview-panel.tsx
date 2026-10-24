import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Eye } from 'lucide-react'
import { useState } from 'react'
import { isApiError } from '@/api/errors'
import { ErrorPanel } from '@/components/error-panel'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { fetchLifecycleRunPage, lifecycleKeys, startCleanupPreview } from '@/features/lifecycle/api'
import { DryRunReport } from '@/features/lifecycle/dry-run-report'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'

export interface PreviewPanelProps {
  /** The saved policy; undefined for a policy that was not saved yet. */
  policyId: string | undefined
  /** The form differs from the saved policy, so the preview would not show what the form says. */
  dirty: boolean
  repositories: readonly string[]
}

/**
 * The dry run of the policy. It runs the SAVED policy (the contract's preview takes a policy id,
 * not a draft), so the button is disabled with its reason while the form is unsaved or dirty.
 * The report appears below it and refreshes while the run is planning; one active run per policy
 * means a second start answers 409 `run_in_progress`, which offers the running one instead.
 */
export function PreviewPanel({ policyId, dirty, repositories }: PreviewPanelProps) {
  const queryClient = useQueryClient()
  const [runId, setRunId] = useState<string | null>(null)
  const [conflict, setConflict] = useState(false)
  const [failure, setFailure] = useState<unknown>(null)

  const start = useMutation({
    mutationFn: (id: string) => startCleanupPreview(id),
    meta: { silent: true },
    onMutate: () => {
      setConflict(false)
      setFailure(null)
    },
    onSuccess: (run) => {
      // The 202 body is the queued run: show it at once, the page polls it from here.
      queryClient.setQueryData(lifecycleKeys.run('cleanup', run.id), run)
      setRunId(run.id)
      toast.success(t('cleanup.preview.started'))
    },
    onError: (error) => {
      if (isApiError(error) && error.code === 'run_in_progress') setConflict(true)
      else setFailure(error)
    },
  })

  async function showRunning(): Promise<void> {
    if (policyId === undefined) return
    const page = await fetchLifecycleRunPage(
      { mode: 'dry', status: undefined, policyId },
      null,
      5,
      new AbortController().signal,
    )
    const active = page.items.find((r) => r.status === 'queued' || r.status === 'running')
    if (active) {
      setRunId(active.id)
      setConflict(false)
    }
  }

  const reasonId = 'preview-reason'
  const reason =
    policyId === undefined
      ? t('cleanup.preview.saveFirst')
      : dirty
        ? t('cleanup.preview.saveChanges')
        : null

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('cleanup.preview.title')}</CardTitle>
        <CardDescription>{t('cleanup.preview.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-1">
          <Button
            variant="outline"
            disabled={reason !== null}
            loading={start.isPending}
            aria-describedby={reason ? reasonId : undefined}
            onClick={() => policyId !== undefined && start.mutate(policyId)}
          >
            <Eye aria-hidden="true" />
            {start.isPending ? t('cleanup.preview.starting') : t('cleanup.preview.start')}
          </Button>
          {reason ? (
            <p id={reasonId} className="text-sm text-muted-foreground">
              {reason}
            </p>
          ) : null}
        </div>
        {conflict ? (
          <Alert variant="warning">
            <span className="flex flex-wrap items-center gap-3">
              {t('cleanup.preview.alreadyRunning')}
              <Button variant="outline" size="sm" onClick={() => void showRunning()}>
                {t('cleanup.preview.showRunning')}
              </Button>
            </span>
          </Alert>
        ) : null}
        {failure !== null ? <ErrorPanel error={failure} /> : null}
        {runId !== null ? <DryRunReport runId={runId} repositories={repositories} /> : null}
      </CardContent>
    </Card>
  )
}
