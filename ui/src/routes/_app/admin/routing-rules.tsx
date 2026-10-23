import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/routing-rules')({
  staticData: { crumb: 'nav.items.routingRules', access: 'admin' },
  component: () => (
    <PlaceholderPage
      titleKey="nav.items.routingRules"
      route="/admin/routing-rules"
      milestone="M1"
    />
  ),
})
