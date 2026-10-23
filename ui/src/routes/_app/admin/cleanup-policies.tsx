import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/cleanup-policies')({
  staticData: { crumb: 'nav.items.cleanupPolicies', access: 'admin' },
  component: () => (
    <PlaceholderPage
      titleKey="nav.items.cleanupPolicies"
      route="/admin/cleanup-policies"
      milestone="M1"
    />
  ),
})
