import { useQuery } from '@tanstack/react-query'
import { Link, Outlet, useParams } from '@tanstack/react-router'
import { Upload } from 'lucide-react'
import { clientSetupKeys } from '@/features/client-setup/api'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { usePermissions } from '@/features/auth/use-permissions'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { failureTitle } from '@/features/components/error-title'
import { toast } from '@/lib/toast-store'
import {
  fetchRepository,
  patchRepository,
  repositoryKeys,
  repositoryQuery,
  type RepositoryView,
} from './api'
import { FormatBadge, OnlineBadge, RepoTypeBadge } from './repo-meta'

interface Tab {
  id: string
  to:
    | '/repositories/$name'
    | '/repositories/$name/browse'
    | '/repositories/$name/components'
    | '/repositories/$name/settings'
    | '/repositories/$name/activity'
  label: string
  exact?: boolean
}

function tabsFor(repo: RepositoryView, canViewAudit: boolean): Tab[] {
  const tabs: Tab[] = [
    { id: 'overview', to: '/repositories/$name', label: t('repos.tabs.overview'), exact: true },
  ]
  // OCI content has no path tree (the API answers 409 `unsupported_for_format`): images are the view.
  if (repo.format !== 'docker' && repo.format !== 'oci') {
    tabs.push({ id: 'browse', to: '/repositories/$name/browse', label: t('pages.browse') })
  }
  const images = repo.format === 'docker' || repo.format === 'oci'
  tabs.push({
    id: 'components',
    to: '/repositories/$name/components',
    label: images ? t('repos.tabs.images') : t('pages.components'),
  })
  if (repo.permissions.admin) {
    tabs.push({
      id: 'settings',
      to: '/repositories/$name/settings',
      label: t('repos.tabs.settings'),
    })
  }
  if (canViewAudit) {
    tabs.push({ id: 'activity', to: '/repositories/$name/activity', label: t('pages.activity') })
  }
  return tabs
}

function RepositoryTabs({ repo, canViewAudit }: { repo: RepositoryView; canViewAudit: boolean }) {
  return (
    <nav aria-label={t('repos.tabs.label')} className="overflow-x-auto border-b border-border">
      <ul className="m-0 flex min-w-max list-none gap-1 p-0">
        {tabsFor(repo, canViewAudit).map((tab) => (
          <li key={tab.id}>
            <Link
              to={tab.to}
              params={{ name: repo.name }}
              activeOptions={{ exact: tab.exact === true }}
              className="focus-inset -mb-px inline-flex h-(--control-h) items-center border-b-2 border-transparent px-3 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors duration-150 hover:text-foreground data-[status=active]:border-accent data-[status=active]:text-foreground"
            >
              {tab.label}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  )
}

/**
 * Shell of every page below `/repositories/:name`: the repository's name as the one page
 * heading, its facts, the online switch for administrators, the tabs, and the page itself.
 * The repository is read once here with its ETag; the tab pages read the same cache entry.
 */
export function RepositoryLayout() {
  const { name } = useParams({ from: '/_app/repositories/$name' })
  const permissions = usePermissions()
  const query = useQuery(repositoryQuery(name))
  const repo = query.data?.data

  const update = useUpdateResource<RepositoryView, boolean>({
    queryKey: repositoryKeys.detail(name),
    resourceName: t('repos.resource', { name }),
    update: (online, etag) => patchRepository(name, { online }, etag),
    fetchCurrent: () => fetchRepository(name),
    invalidate: [repositoryKeys.all, clientSetupKeys.all],
    optimistic: (current, online) => ({ ...current, online }),
    copyChanges: (online) => JSON.stringify({ online }),
    onSuccess: (saved) =>
      toast.success(saved.online ? t('repos.onlineOn', { name }) : t('repos.onlineOff', { name })),
  })

  if (repo === undefined) {
    return (
      <>
        <PageHeader title={name} />
        {query.error ? (
          <ErrorPanel
            error={query.error}
            title={failureTitle(query.error, t('repos.loadFailed'))}
            onRetry={() => void query.refetch()}
          />
        ) : (
          <div aria-busy="true" className="space-y-3">
            <Skeleton className="h-9 w-72" />
            <Skeleton className="h-40" />
          </div>
        )}
      </>
    )
  }

  const canUpload = repo.permissions.write && repo.repo_type === 'hosted'
  return (
    <>
      <PageHeader
        title={repo.name}
        description={
          <span className="flex flex-wrap items-center gap-1.5">
            <FormatBadge format={repo.format} />
            <RepoTypeBadge type={repo.repo_type} />
            <OnlineBadge online={repo.online} />
          </span>
        }
        actions={
          <>
            {canUpload ? (
              <Button asChild variant="outline">
                <Link to="/upload" search={{ repository: repo.name }}>
                  <Upload aria-hidden="true" />
                  {t('repos.upload')}
                </Link>
              </Button>
            ) : null}
            {repo.permissions.admin ? (
              <div className="flex items-center gap-2">
                <Switch
                  id="repository-online"
                  checked={repo.online}
                  disabled={update.isPending}
                  onCheckedChange={(online) => update.mutate(online)}
                />
                <Label htmlFor="repository-online">{t('repos.onlineSwitch')}</Label>
              </div>
            ) : null}
          </>
        }
      />
      {!repo.online ? (
        <Alert variant="warning" className="mb-3">
          {t('repos.offlineBanner')} {repo.permissions.admin ? t('repos.offlineBannerAdmin') : null}
        </Alert>
      ) : null}
      <RepositoryTabs repo={repo} canViewAudit={permissions.can.view_audit} />
      <div className="pt-4">
        <Outlet />
      </div>
      <ConflictDialog {...update.conflict} />
    </>
  )
}
