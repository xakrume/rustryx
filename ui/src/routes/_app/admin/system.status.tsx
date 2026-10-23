import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/system/status')({
  staticData: { crumb: 'nav.items.status', access: { area: 'system' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.status" route="/admin/system/status" milestone="M1" />
  ),
})
