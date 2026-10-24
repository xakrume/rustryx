import { createFileRoute } from '@tanstack/react-router'
import { AuditPage } from '@/features/audit/audit-page'
import { pickText } from '@/features/roles/search-params'

export const Route = createFileRoute('/_app/admin/audit')({
  staticData: { crumb: 'nav.items.auditLog', access: { area: 'audit' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, [
      'kind',
      'actor',
      'action',
      'target',
      'repository',
      'project',
      'request_id',
      'outcome',
      'range',
      'from',
      'to',
      'sort',
      'event',
      'export',
    ] as const),
  component: AuditPage,
})
