import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/settings')({
  staticData: { crumb: 'nav.items.settings', access: { area: 'settings' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.settings" route="/admin/settings" milestone="M1" />
  ),
})
