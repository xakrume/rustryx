import { Link } from '@tanstack/react-router'
import { CircleCheck, RotateCcw, X } from 'lucide-react'
import { isApiError } from '@/api/errors'
import type { components } from '@/api/schema'
import { BytesText } from '@/components/bytes-text'
import { ErrorPanel } from '@/components/error-panel'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { componentName } from '@/features/components/api'
import { Meter } from '@/features/components/meter'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import type { PickedFile } from './upload-model'

type UploadResult = components['schemas']['UploadResult']

export interface ProgressPanelProps {
  files: readonly PickedFile[]
  /** 0 to 1 per file, in order. */
  progress: readonly number[]
  onCancel: () => void
}

/**
 * Progress of the upload, per file and overall, each as a real progressbar with its value, and
 * a Cancel button that is the first thing reachable by keyboard from here. Cancelling aborts the
 * request; the server removes what it already staged, so nothing half published is left.
 */
export function ProgressPanel({ files, progress, onCancel }: ProgressPanelProps) {
  const total = files.reduce((sum, f) => sum + f.file.size, 0)
  const overall =
    files.length === 0
      ? 0
      : total === 0
        ? progress.reduce((sum, p) => sum + p, 0) / files.length
        : files.reduce((sum, f, i) => sum + f.file.size * (progress[i] ?? 0), 0) / total
  const percent = Math.round(overall * 100)
  return (
    <section
      aria-label={t('upload.progressTitle')}
      aria-busy="true"
      className="space-y-3 rounded-xl border border-transparent bg-card dark:border-border/60 p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-base font-semibold">{t('upload.progressTitle')}</h2>
        <Button variant="outline" size="sm" onClick={onCancel}>
          <X aria-hidden="true" />
          {t('upload.cancel')}
        </Button>
      </div>
      <div className="space-y-1">
        <Meter percent={percent} label={t('upload.overall')} />
        <p className="text-xs text-muted-foreground" role="status">
          {t('upload.percent', {
            percent,
            sent: formatBytes(total * overall),
            total: formatBytes(total),
          })}
        </p>
      </div>
      <ul className="m-0 list-none space-y-2 p-0 text-sm">
        {files.map(({ id, file }, i) => {
          const value = Math.round((progress[i] ?? 0) * 100)
          return (
            <li key={id} className="space-y-1">
              <div className="flex items-center justify-between gap-2">
                <span className="min-w-0 truncate font-mono">{file.name}</span>
                <span className="text-muted-foreground tabular-nums">{value}%</span>
              </div>
              <Meter
                percent={value}
                label={t('upload.fileProgress', { name: file.name })}
                className="h-1.5"
              />
            </li>
          )
        })}
      </ul>
    </section>
  )
}

interface Explained {
  title: string
  advice: string
}

/** The inventory's upload failures, said in the user's terms; any other failure falls back to the generic panel. */
export function explainUploadError(error: unknown): Explained | null {
  if (!isApiError(error)) return null
  const details = error.details ?? {}
  switch (error.code) {
    case 'write_policy_violation':
      return {
        title: t('upload.explain.policyTitle'),
        advice:
          details.policy === 'deny'
            ? t('upload.explain.policyDeny')
            : t('upload.explain.policyOnce'),
      }
    case 'quota_exceeded': {
      const limit = typeof details.limit_bytes === 'number' ? formatBytes(details.limit_bytes) : '-'
      const used = typeof details.used_bytes === 'number' ? formatBytes(details.used_bytes) : '-'
      return {
        title: t('upload.explain.quotaTitle'),
        advice: t('upload.explain.quota', { used, limit }),
      }
    }
    case 'digest_mismatch':
      return { title: t('upload.explain.digestTitle'), advice: t('upload.explain.digest') }
    case 'payload_too_large':
      return { title: t('upload.explain.tooLargeTitle'), advice: t('upload.explain.tooLarge') }
    case 'repository_not_hosted':
      return { title: t('upload.explain.notHostedTitle'), advice: t('upload.explain.notHosted') }
    case 'network_error':
      return {
        title: t('upload.explain.interruptedTitle'),
        advice: t('upload.explain.interrupted'),
      }
    default:
      return null
  }
}

export interface UploadErrorPanelProps {
  error: unknown
  /** Retry the same upload with the same idempotency key. */
  onRetry: () => void
}

/** A failed upload: what happened in plain words, the stable code and request id, and Retry. */
export function UploadErrorPanel({ error, onRetry }: UploadErrorPanelProps) {
  const explained = explainUploadError(error)
  return (
    <div className="space-y-2">
      <ErrorPanel error={error} title={explained?.title ?? t('upload.failedTitle')} />
      {explained ? <Alert variant="info">{explained.advice}</Alert> : null}
      <Button variant="outline" onClick={onRetry}>
        <RotateCcw aria-hidden="true" />
        {t('upload.retry')}
      </Button>
    </div>
  )
}

/** The outcome: the new component with links, what it replaced, and the next steps. */
export function ResultPanel({
  result,
  onAnother,
}: {
  result: UploadResult
  onAnother: () => void
}) {
  const { component, assets, replaced } = result
  return (
    <section
      aria-label={t('upload.resultTitle')}
      className="space-y-3 rounded-lg border border-success bg-card p-4"
    >
      <h2 className="flex items-center gap-2 text-base font-semibold text-success-text">
        <CircleCheck className="size-5" aria-hidden="true" />
        {t('upload.resultTitle')}
      </h2>
      <p className="text-sm">
        {t('upload.published', { count: assets.length })}{' '}
        <Link
          to="/components/$id"
          params={{ id: component.id }}
          className="font-mono text-link underline underline-offset-2"
        >
          {componentName(component)}
          {component.version ? ` ${component.version}` : ''}
        </Link>{' '}
        {t('upload.inRepository')}{' '}
        <Link
          to="/repositories/$name"
          params={{ name: component.repository }}
          className="font-mono text-link underline underline-offset-2"
        >
          {component.repository}
        </Link>
      </p>
      <ul className="m-0 list-none space-y-1 p-0 text-sm">
        {assets.map((asset) => (
          <li key={asset.id} className="flex flex-wrap items-baseline gap-x-3">
            <Link
              to="/assets/$id"
              params={{ id: asset.id }}
              className="font-mono text-[0.8125rem] break-all text-link underline-offset-2 hover:underline"
            >
              {asset.path}
            </Link>
            <BytesText bytes={asset.size_bytes} className="text-muted-foreground" />
          </li>
        ))}
      </ul>
      {replaced.length > 0 ? (
        <Alert variant="warning" title={t('upload.replacedTitle')}>
          <ul className="m-0 list-disc pl-5 font-mono text-[0.8125rem]">
            {replaced.map((path) => (
              <li key={path} className="break-all">
                {path}
              </li>
            ))}
          </ul>
        </Alert>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <Button onClick={onAnother}>{t('upload.another')}</Button>
        <Button asChild variant="outline">
          <Link
            to="/repositories/$name/components"
            params={{ name: component.repository }}
            search={{}}
          >
            {t('upload.openComponents')}
          </Link>
        </Button>
      </div>
    </section>
  )
}
