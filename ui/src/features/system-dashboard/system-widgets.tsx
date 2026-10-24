import { PrivilegeGate } from '@/components/privilege-gate'
import { JobHealthWidget } from './jobs-widget'
import { RequestSummaryWidget } from './request-widget'
import { StorageTrendWidget } from './storage-widget'

/**
 * The system widgets as one block for the dashboard (docs/ui-page-inventory.md, 7.1): storage
 * trend, request and error summary, job health. They need administrator data, so the block is
 * hidden for everyone else instead of showing three "forbidden" cards. The dashboard owner can
 * also place the three widgets one by one (`StorageTrendWidget`, `RequestSummaryWidget`,
 * `JobHealthWidget`); each loads, fails and retries on its own.
 */
export function SystemDashboardWidgets({ className }: { className?: string }) {
  return (
    <PrivilegeGate need="admin">
      <div className={className ?? 'grid gap-4 xl:grid-cols-2'}>
        <StorageTrendWidget className="xl:col-span-2" />
        <RequestSummaryWidget />
        <JobHealthWidget />
      </div>
    </PrivilegeGate>
  )
}
