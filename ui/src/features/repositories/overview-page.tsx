import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from '@tanstack/react-router'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { Fact, FactList } from '@/features/components/fact-list'
import { Meter, quotaTone } from '@/features/components/meter'
import { t } from '@/i18n'
import {
  recentComponentsQuery,
  repositoryNamesQuery,
  repositoryQuery,
  type RepositoryView,
} from './api'
import { OnlineBadge, RepoTypeBadge, UpstreamBadge } from './repo-meta'

function Summary({ repo }: { repo: RepositoryView }) {
  const anonymous = repo.anonymous_effective
    ? t('repos.overview.anonymousOn')
    : repo.allow_anonymous
      ? t('repos.overview.anonymousBlockedGlobally')
      : t('repos.overview.anonymousOff')
  return (
    <Card className="lg:col-span-2">
      <CardHeader>
        <CardTitle>{t('repos.overview.summary')}</CardTitle>
      </CardHeader>
      <CardContent>
        <FactList>
          <Fact label={t('repos.columns.url')}>
            <span className="flex items-center gap-1">
              <span className="font-mono text-[0.8125rem] break-all">{repo.url}</span>
              <CopyButton value={repo.url} what={t('repos.copyUrlOf', { name: repo.name })} />
            </span>
          </Fact>
          <Fact label={t('repos.columns.blobStore')} mono>
            {repo.blob_store.name}
          </Fact>
          {repo.repo_type === 'hosted' ? (
            <Fact label={t('repoForm.storage.writePolicy')}>
              {t(`repoForm.storage.policy.${repo.write_policy}`)}
            </Fact>
          ) : null}
          <Fact label={t('repos.overview.anonymous')}>{anonymous}</Fact>
          <Fact label={t('repoForm.storage.quota')}>
            {repo.quota_bytes === null ? (
              t('repoForm.review.noQuota')
            ) : (
              <BytesText bytes={repo.quota_bytes} />
            )}
          </Fact>
          <Fact label={t('repoForm.policies.cleanup')}>
            {repo.cleanup_policies.length === 0
              ? t('common.none')
              : repo.cleanup_policies.map((p) => p.name).join(', ')}
          </Fact>
          {repo.repo_type !== 'hosted' ? (
            <Fact label={t('repoForm.policies.routingRule')}>
              {repo.routing_rule?.name ?? t('common.none')}
            </Fact>
          ) : null}
          <Fact label={t('repos.columns.created')}>
            <RelativeTime value={repo.created_at} />
          </Fact>
        </FactList>
      </CardContent>
    </Card>
  )
}

function Usage({ repo }: { repo: RepositoryView }) {
  const stats = repo.stats
  const used = stats?.size_bytes ?? 0
  const percent =
    repo.quota_bytes !== null && repo.quota_bytes > 0
      ? Math.min(100, Math.round((used / repo.quota_bytes) * 100))
      : null
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('repos.overview.usage')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {stats ? (
          <>
            <p className="text-2xl font-semibold tabular">
              <BytesText bytes={stats.size_bytes} />
            </p>
            <FactList>
              <Fact label={t('repos.columns.components')}>
                {stats.component_count.toLocaleString()}
              </Fact>
              <Fact label={t('repos.overview.assets')}>{stats.asset_count.toLocaleString()}</Fact>
              <Fact label={t('repos.overview.asOf')}>
                <RelativeTime value={stats.as_of} />
              </Fact>
            </FactList>
            {percent !== null ? (
              <div className="space-y-1">
                <Meter
                  percent={percent}
                  tone={quotaTone(percent)}
                  label={t('repos.overview.quotaUsed')}
                />
                <p className="text-xs text-muted-foreground">
                  {t('repos.overview.quotaPercent', { percent })}
                </p>
              </div>
            ) : null}
          </>
        ) : (
          <p className="text-sm text-muted-foreground">{t('repos.overview.noStats')}</p>
        )}
      </CardContent>
    </Card>
  )
}

function Upstream({ repo }: { repo: RepositoryView }) {
  if (repo.proxy === null) return null
  const upstream = repo.upstream
  return (
    <Card className="lg:col-span-3">
      <CardHeader>
        <CardTitle>{t('repoForm.upstream.title')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <FactList>
          <Fact label={t('repoForm.proxy.remoteUrl')} mono>
            {repo.proxy.remote_url}
          </Fact>
          <Fact label={t('repos.columns.status')}>
            {upstream ? <UpstreamBadge state={upstream.state} /> : t('common.unknown')}
          </Fact>
          {upstream?.last_error_code ? (
            <Fact label={t('repoForm.upstream.lastError')} mono>
              {upstream.last_error_code}
            </Fact>
          ) : null}
          {upstream ? (
            <Fact label={t('repoForm.upstream.checked')}>
              <RelativeTime value={upstream.last_checked_at} />
            </Fact>
          ) : null}
          <Fact label={t('repoForm.proxy.credentials')}>
            {repo.proxy.auth
              ? t('repos.overview.credentialsAs', { username: repo.proxy.auth.username })
              : t('repoForm.review.noCredentials')}
          </Fact>
        </FactList>
        {upstream && upstream.state !== 'ok' ? (
          <Alert variant="warning">{t('repos.overview.serveStale')}</Alert>
        ) : null}
      </CardContent>
    </Card>
  )
}

function Members({ repo }: { repo: RepositoryView }) {
  const names = useQuery(repositoryNamesQuery)
  if (repo.group === null) return null
  const known = new Map((names.data?.items ?? []).map((r) => [r.name, r]))
  const offline = repo.group.members.filter((m) => known.get(m)?.online === false)
  return (
    <Card className="lg:col-span-3">
      <CardHeader>
        <CardTitle>{t('repos.overview.members')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <p className="text-sm text-muted-foreground">{t('repos.overview.membersBody')}</p>
        <ol className="m-0 list-none divide-y divide-border p-0 text-sm">
          {repo.group.members.map((member, index) => {
            const info = known.get(member)
            return (
              <li key={member} className="flex flex-wrap items-center gap-2 py-1.5">
                <span className="w-6 text-right text-muted-foreground tabular-nums">
                  {index + 1}
                </span>
                <Link
                  to="/repositories/$name"
                  params={{ name: member }}
                  className="font-mono text-link underline-offset-2 hover:underline"
                >
                  {member}
                </Link>
                {info ? <RepoTypeBadge type={info.repo_type} /> : null}
                {info ? <OnlineBadge online={info.online} /> : null}
              </li>
            )
          })}
        </ol>
        {offline.length > 0 ? (
          <Alert variant="warning">
            {t('repos.overview.memberOffline', { names: offline.join(', ') })}
          </Alert>
        ) : null}
      </CardContent>
    </Card>
  )
}

function Recent({ repo }: { repo: RepositoryView }) {
  const recent = useQuery(recentComponentsQuery(repo.name))
  return (
    <Card className="lg:col-span-3">
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('repos.overview.recent')}</CardTitle>
        <Link
          to="/repositories/$name/components"
          params={{ name: repo.name }}
          className="text-sm text-link underline-offset-2 hover:underline"
        >
          {t('repos.overview.viewAll')}
        </Link>
      </CardHeader>
      <CardContent>
        {recent.error ? (
          <ErrorPanel error={recent.error} onRetry={() => void recent.refetch()} />
        ) : recent.data === undefined ? (
          <Skeleton className="h-24" aria-hidden="true" />
        ) : recent.data.items.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {repo.repo_type === 'hosted'
              ? t('repos.overview.emptyHosted')
              : t('repos.overview.emptyOther')}
          </p>
        ) : (
          <ul className="m-0 list-none divide-y divide-border p-0 text-sm">
            {recent.data.items.map((component) => (
              <li key={component.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                <Link
                  to="/components/$id"
                  params={{ id: component.id }}
                  className="font-mono text-link underline-offset-2 hover:underline"
                >
                  {component.group_id ? `${component.group_id}:` : ''}
                  {component.name}
                </Link>
                {component.version ? (
                  <span className="font-mono text-muted-foreground">{component.version}</span>
                ) : null}
                <RelativeTime
                  value={component.created_at}
                  className="ml-auto text-muted-foreground"
                />
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}

/** Overview tab: facts, usage, the upstream or the members, recent components, client setup. */
export function RepositoryOverviewPage() {
  const { name } = useParams({ from: '/_app/repositories/$name/' })
  const repo = useQuery(repositoryQuery(name)).data?.data
  // The layout above shows the loading and error states of the repository itself.
  if (repo === undefined) return null
  return (
    <div className="grid gap-4 lg:grid-cols-3">
      <h2 className="sr-only">{t('repos.tabs.overview')}</h2>
      <Summary repo={repo} />
      <Usage repo={repo} />
      <Upstream repo={repo} />
      <Members repo={repo} />
      <Recent repo={repo} />
      <Card className="lg:col-span-3">
        <CardContent className="p-4">
          <ClientSetupPanel repository={repo.name} />
        </CardContent>
      </Card>
    </div>
  )
}
