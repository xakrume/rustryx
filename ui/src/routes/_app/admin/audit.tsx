import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/audit')({
  staticData: { crumb: 'nav.items.auditLog', access: { area: 'audit' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.auditLog" route="/admin/audit" milestone="M1" />
  ),
})
