import { useMutation } from '@tanstack/react-query'
import { Plug } from 'lucide-react'
import { useFormContext } from 'react-hook-form'
import { CheckboxField, TextField } from '@/components/form/form'
import { StatusBadge } from '@/components/status-badge'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'
import { formatDuration } from '@/lib/format'
import { checkUpstream, type RepoFormat, type UpstreamCheckResult } from '../api'
import { FormSection } from './sections'
import type { RepositoryFormValues } from './values'

type Values = RepositoryFormValues

function ageHint(seconds: string): string | undefined {
  return /^\d+$/.test(seconds) && Number(seconds) > 0
    ? t('repoForm.proxy.equals', { duration: formatDuration(Number(seconds)) })
    : undefined
}

function ProbeResult({ result }: { result: UpstreamCheckResult }) {
  if (result.ok) {
    return (
      <Alert variant="success">
        {t('repoForm.proxy.probeOk', {
          latency: result.latency_ms ?? 0,
          status: result.http_status ?? 200,
        })}
      </Alert>
    )
  }
  return (
    <Alert variant="warning" title={t('repoForm.proxy.probeFailed')}>
      <span className="block">
        {t('repoForm.proxy.probeFailedBody')}{' '}
        {result.error_code ? <code className="font-mono">{result.error_code}</code> : null}
        {result.rule ? (
          <>
            {' '}
            {t('repoForm.proxy.probeRule')} <code className="font-mono">{result.rule}</code>
          </>
        ) : null}
      </span>
    </Alert>
  )
}

/**
 * Upstream of a proxy repository: the remote URL (checked by the server's SSRF filter on save,
 * with the rule named when it refuses), optional credentials with a write only password, and the
 * freshness settings. "Test connection" probes the upstream with what is typed; a failed probe is
 * a warning, never a block (docs/ui-page-inventory.md, 7.2).
 */
export function ProxySection({
  mode,
  hasStoredAuth,
}: {
  mode: 'create' | 'edit'
  hasStoredAuth: boolean
}) {
  const { watch, getValues } = useFormContext<Values>()
  const probe = useMutation({ meta: { silent: true }, mutationFn: checkUpstream })
  const [content, metadata, negative] = watch([
    'content_max_age_secs',
    'metadata_max_age_secs',
    'negative_cache_ttl_secs',
  ])
  const removing = watch('remove_credentials')

  function test(): void {
    const v = getValues()
    probe.mutate({
      format: v.format as RepoFormat,
      remote_url: v.remote_url.trim(),
      auth:
        v.auth_username.trim() === ''
          ? null
          : { username: v.auth_username.trim(), password: v.auth_password || undefined },
    })
  }

  return (
    <>
      <FormSection title={t('repoForm.proxy.title')} description={t('repoForm.proxy.body')}>
        <TextField<Values>
          name="remote_url"
          label={t('repoForm.proxy.remoteUrl')}
          helper={t('repoForm.proxy.remoteUrlHelper')}
          type="url"
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          placeholder="https://registry.example.org"
          required
        />
        <div className="flex flex-wrap items-center gap-3">
          <Button variant="outline" loading={probe.isPending} onClick={test}>
            <Plug aria-hidden="true" />
            {t('repoForm.proxy.test')}
          </Button>
          <span className="text-xs text-muted-foreground">{t('repoForm.proxy.testNote')}</span>
        </div>
        <div aria-live="polite">
          {probe.data ? <ProbeResult result={probe.data} /> : null}
          {probe.error ? (
            <Alert variant="warning">{t('repoForm.proxy.probeUnavailable')}</Alert>
          ) : null}
        </div>
      </FormSection>

      <FormSection
        title={t('repoForm.proxy.credentials')}
        description={t('repoForm.proxy.credentialsBody')}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <TextField<Values>
            name="auth_username"
            label={t('repoForm.proxy.username')}
            autoComplete="off"
            spellCheck={false}
            disabled={removing}
          />
          <TextField<Values>
            name="auth_password"
            label={t('repoForm.proxy.password')}
            type="password"
            autoComplete="new-password"
            disabled={removing}
            placeholder={
              hasStoredAuth && mode === 'edit' ? t('repoForm.proxy.passwordStored') : undefined
            }
            helper={
              hasStoredAuth && mode === 'edit'
                ? t('repoForm.proxy.passwordKeep')
                : t('repoForm.proxy.passwordWriteOnly')
            }
          />
        </div>
        {hasStoredAuth && mode === 'edit' ? (
          <div className="flex items-center gap-2">
            <StatusBadge status="info" label={t('repoForm.proxy.credentialsSet')} />
            <CheckboxField<Values>
              name="remove_credentials"
              label={t('repoForm.proxy.removeCredentials')}
            />
          </div>
        ) : null}
      </FormSection>

      <FormSection
        title={t('repoForm.proxy.freshness')}
        description={t('repoForm.proxy.freshnessBody')}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <TextField<Values>
            name="content_max_age_secs"
            label={t('repoForm.proxy.contentAge')}
            helper={ageHint(content) ?? t('repoForm.proxy.contentAgeHelper')}
            inputMode="numeric"
          />
          <TextField<Values>
            name="metadata_max_age_secs"
            label={t('repoForm.proxy.metadataAge')}
            helper={ageHint(metadata) ?? t('repoForm.proxy.metadataAgeHelper')}
            inputMode="numeric"
          />
          <TextField<Values>
            name="negative_cache_ttl_secs"
            label={t('repoForm.proxy.negativeTtl')}
            helper={ageHint(negative) ?? t('repoForm.proxy.negativeTtlHelper')}
            inputMode="numeric"
          />
          <TextField<Values>
            name="max_concurrent_upstream"
            label={t('repoForm.proxy.concurrency')}
            helper={t('repoForm.proxy.concurrencyHelper')}
            inputMode="numeric"
          />
        </div>
      </FormSection>
    </>
  )
}
