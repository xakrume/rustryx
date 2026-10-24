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
        <Summary summary={data} />
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
