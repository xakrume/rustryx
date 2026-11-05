import { useQuery } from '@tanstack/react-query'
import { BytesText } from '@/components/bytes-text'
import { t } from '@/i18n'
import { formatDuration } from '@/lib/format'
import { blobStoresQuery, cleanupPoliciesQuery, routingRulesQuery } from '../api'
import { formatLabel } from '../repo-meta'
import { humanize, type ConfigField } from './format-config'
import { GIB, type RepositoryFormValues } from './values'

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-1 py-2 sm:grid-cols-[14rem_1fr] sm:gap-4">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  )
}

const yesNo = (value: boolean): string => (value ? t('common.yes') : t('common.no'))

/**
 * The last wizard step: what will be created, in the words of the form, before anything is sent.
 * The password is never shown: it appears only as "set".
 */
export function ReviewStep({
  values: v,
  fields,
}: {
  values: RepositoryFormValues
  fields: readonly ConfigField[]
}) {
  const stores = useQuery(blobStoresQuery).data?.items ?? []
  const rules = useQuery(routingRulesQuery).data?.items ?? []
  const policies = useQuery(cleanupPoliciesQuery).data?.items ?? []
  const nameOf = (list: readonly { id: string; name: string }[], id: string): string =>
    list.find((item) => item.id === id)?.name ?? id
  return (
    <section className="rounded-xl border border-transparent bg-card dark:border-border/60 p-4">
      <h2 className="text-base font-semibold">{t('repoForm.review.title')}</h2>
      <p className="text-sm text-muted-foreground">{t('repoForm.review.body')}</p>
      <dl className="mt-2 divide-y divide-border text-sm">
        <Row label={t('repoForm.identity.name')}>
          <span className="font-mono">{v.name}</span>
        </Row>
        <Row label={t('repoForm.identity.format')}>{formatLabel(v.format)}</Row>
        <Row label={t('repoForm.identity.type')}>{t(`repos.type.${v.repo_type}`)}</Row>
        <Row label={t('repoForm.storage.blobStore')}>
          <span className="font-mono">{nameOf(stores, v.blob_store_id)}</span>
        </Row>
        {v.repo_type === 'hosted' ? (
          <Row label={t('repoForm.storage.writePolicy')}>
            {t(`repoForm.storage.policy.${v.write_policy}`)}
          </Row>
        ) : null}
        <Row label={t('repoForm.storage.quota')}>
          {v.quota_gib === '' ? (
            t('repoForm.review.noQuota')
          ) : (
            <BytesText bytes={Number(v.quota_gib) * GIB} />
          )}
        </Row>
        <Row label={t('repoForm.access.anonymous')}>{yesNo(v.allow_anonymous)}</Row>
        <Row label={t('repoForm.access.online')}>{yesNo(v.online)}</Row>
        {v.repo_type === 'proxy' ? (
          <>
            <Row label={t('repoForm.proxy.remoteUrl')}>
              <span className="font-mono">{v.remote_url}</span>
            </Row>
            <Row label={t('repoForm.proxy.credentials')}>
              {v.auth_username.trim() === '' ? (
                t('repoForm.review.noCredentials')
              ) : (
                <>
                  <span className="font-mono">{v.auth_username}</span>
                  {', '}
                  {v.auth_password === ''
                    ? t('repoForm.review.passwordNone')
                    : t('repoForm.review.passwordSet')}
                </>
              )}
            </Row>
            <Row label={t('repoForm.proxy.freshness')}>
              {t('repoForm.review.freshness', {
                content: formatDuration(Number(v.content_max_age_secs)),
                metadata: formatDuration(Number(v.metadata_max_age_secs)),
                negative: formatDuration(Number(v.negative_cache_ttl_secs)),
              })}
            </Row>
          </>
        ) : null}
        {v.repo_type === 'group' ? (
          <Row label={t('repoForm.group.title')}>
            <ol className="m-0 list-decimal pl-5">
              {v.members.map((m) => (
                <li key={m} className="font-mono">
                  {m}
                </li>
              ))}
            </ol>
          </Row>
        ) : null}
        {v.repo_type !== 'hosted' ? (
          <Row label={t('repoForm.policies.routingRule')}>
            {v.routing_rule_id === '' ? (
              t('common.none')
            ) : (
              <span className="font-mono">{nameOf(rules, v.routing_rule_id)}</span>
            )}
          </Row>
        ) : null}
        <Row label={t('repoForm.policies.cleanup')}>
          {v.cleanup_policy_ids.length === 0
            ? t('common.none')
            : v.cleanup_policy_ids.map((id) => nameOf(policies, id)).join(', ')}
        </Row>
        {fields.map((field) => (
          <Row key={field.key} label={field.label}>
            {typeof v.config[field.key] === 'boolean'
              ? yesNo(v.config[field.key] === true)
              : field.kind === 'enum'
                ? humanize(String(v.config[field.key] ?? ''))
                : String(v.config[field.key] ?? '')}
          </Row>
        ))}
      </dl>
    </section>
  )
}
