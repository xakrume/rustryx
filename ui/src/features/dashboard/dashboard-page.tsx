import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { KeyRound, Plus, Upload } from 'lucide-react'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { usePermissions } from '@/features/auth/use-permissions'
import { useDelayedFlag } from '@/hooks/use-delayed-flag'
import { t } from '@/i18n'
import { dashboardQuery, type DashboardSummary } from './api'
import {
  ActivityCard,
  FailedTasksCard,
  RepositoriesCard,
  SetupCard,
  StorageCard,
} from './dashboard-cards'

function Summary({ summary }: { summary: DashboardSummary }) {
  const permissions = usePermissions()
  const open = summary.setup_checklist?.filter((step) => !step.done) ?? []
  return (
    <div className="space-y-4">
      {summary.degraded === true ? (
        <Alert variant="warning">
          {t('shell.banners.degraded')}{' '}
          <Link to="/admin/system/status" className="font-medium underline underline-offset-2">
            {t('shell.banners.degradedLink')}
          </Link>
        </Alert>
      ) : null}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        <RepositoriesCard summary={summary.repositories} />
        {summary.storage ? <StorageCard stores={summary.storage} /> : null}
        {open.length > 0 && summary.setup_checklist ? (
          <SetupCard steps={summary.setup_checklist} />
        ) : null}
        {summary.failed_tasks && summary.failed_tasks.length > 0 ? (
          <FailedTasksCard runs={summary.failed_tasks} />
        ) : null}
        {summary.recent_activity ? (
          <ActivityCard events={summary.recent_activity} showAll={permissions.can.view_audit} />
        ) : null}
      </div>
    </div>
  )
}

interface JobLink {
  to:
    | '/search'
    | '/repositories'
    | '/upload'
    | '/admin/blob-stores'
    | '/admin/cleanup-policies'
    | '/admin/routing-rules'
    | '/admin/tokens'
    | '/admin/tasks'
    | '/admin/users'
  label: string
  hint: string
  search?: Record<string, never>
}

function JobGroup({ title, jobs }: { title: string; jobs: JobLink[] }) {
  if (jobs.length === 0) return null
  return (
    <div>
      <h3 className="text-sm font-semibold">{title}</h3>
      <ul className="mt-2 space-y-3">
        {jobs.map((job) => (
          <li key={job.label}>
            <Link
              to={job.to}
              search={job.search ?? {}}
              className="font-medium text-link underline-offset-2 hover:underline"
            >
              {job.label}
            </Link>
            <p className="text-sm text-muted-foreground">{job.hint}</p>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** The jobs an engineer has on both a package manager and an image registry, on the screens that exist. */
function EngineerJobs() {
  const permissions = usePermissions()
  if (!permissions.authenticated) return null
  const find: JobLink[] = [
    {
      to: '/search',
      label: t('dashboard.jobs.search'),
      hint: t('dashboard.jobs.searchHint'),
    },
    {
      to: '/repositories',
      label: t('dashboard.jobs.repositories'),
      hint: t('dashboard.jobs.repositoriesHint'),
    },
  ]
  const publish: JobLink[] = [
    ...(permissions.can.upload
      ? [
          {
            to: '/upload' as const,
            label: t('dashboard.jobs.upload'),
            hint: t('dashboard.jobs.uploadHint'),
          },
        ]
      : []),
    {
      to: '/repositories',
      label: t('dashboard.jobs.setup'),
      hint: t('dashboard.jobs.setupHint'),
    },
  ]
  const run: JobLink[] = [
    ...(permissions.hasArea('blobstores')
      ? [
          {
            to: '/admin/blob-stores' as const,
            label: t('dashboard.jobs.blobStores'),
            hint: t('dashboard.jobs.blobStoresHint'),
          },
        ]
      : []),
    ...(permissions.isAdmin
      ? [
          {
            to: '/admin/cleanup-policies' as const,
            label: t('dashboard.jobs.cleanup'),
            hint: t('dashboard.jobs.cleanupHint'),
          },
          {
            to: '/admin/routing-rules' as const,
            label: t('dashboard.jobs.routing'),
            hint: t('dashboard.jobs.routingHint'),
          },
        ]
      : []),
    ...(permissions.hasArea('tokens')
      ? [
          {
            to: '/admin/tokens' as const,
            label: t('dashboard.jobs.tokens'),
            hint: t('dashboard.jobs.tokensHint'),
          },
        ]
      : []),
    ...(permissions.hasArea('tasks')
      ? [
          {
            to: '/admin/tasks' as const,
            label: t('dashboard.jobs.tasks'),
            hint: t('dashboard.jobs.tasksHint'),
          },
        ]
      : []),
    ...(permissions.hasArea('users')
      ? [
          {
            to: '/admin/users' as const,
            label: t('dashboard.jobs.users'),
            hint: t('dashboard.jobs.usersHint'),
          },
        ]
      : []),
  ]
  return (
    <section aria-labelledby="engineer-jobs" className="mb-6 rounded-lg border border-border p-4">
      <h2 id="engineer-jobs" className="text-base font-semibold">
        {t('dashboard.jobs.title')}
      </h2>
      <p className="mt-1 max-w-3xl text-sm text-muted-foreground">{t('dashboard.jobs.lead')}</p>
      <div className="mt-4 grid gap-6 md:grid-cols-3">
        <JobGroup title={t('dashboard.jobs.find')} jobs={find} />
        <JobGroup title={t('dashboard.jobs.publish')} jobs={publish} />
        <JobGroup title={t('dashboard.jobs.run')} jobs={run} />
      </div>
    </section>
  )
}

/** Quick actions of the home page: what a signed in user most often wants to do next. */
function QuickActions() {
  const permissions = usePermissions()
  return (
    <>
      {permissions.can.create_repository ? (
        <Button asChild>
          <Link to="/repositories/new">
            <Plus aria-hidden="true" />
            {t('repos.create')}
          </Link>
        </Button>
      ) : null}
      {permissions.can.upload ? (
        <Button asChild variant="outline">
          <Link to="/upload" search={{}}>
            <Upload aria-hidden="true" />
            {t('nav.items.upload')}
          </Link>
        </Button>
      ) : null}
      {permissions.can.create_personal_token ? (
        <Button asChild variant="outline">
          <Link to="/me/tokens">
            <KeyRound aria-hidden="true" />
            {t('home.createToken')}
          </Link>
        </Button>
      ) : null}
    </>
  )
}

export function DashboardPage() {
  const { data, isPending, error, refetch } = useQuery(dashboardQuery)
  const showSkeleton = useDelayedFlag(isPending, 150)
  const permissions = usePermissions()
  return (
    <>
      <PageHeader
        title={t('pages.home')}
        description={t('dashboard.description')}
        actions={permissions.authenticated ? <QuickActions /> : undefined}
      />
      {data?.public_only ? (
        <Alert variant="info" className="mb-4">
          {t('dashboard.publicOnly')}{' '}
          <Link to="/login" className="font-medium underline underline-offset-2">
            {t('shell.userMenu.signIn')}
          </Link>
        </Alert>
      ) : null}
      {error && !data ? (
        <ErrorPanel
          error={error}
          onRetry={() => void refetch()}
          title={t('dashboard.loadFailed')}
        />
      ) : data ? (
        <>
          <EngineerJobs />
          <Summary summary={data} />
        </>
      ) : (
        <div aria-busy="true" className="grid min-h-48 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {showSkeleton ? (
            <>
              <Skeleton className="h-48" />
              <Skeleton className="h-48" />
              <Skeleton className="h-48" />
            </>
          ) : null}
        </div>
      )}
    </>
  )
}
