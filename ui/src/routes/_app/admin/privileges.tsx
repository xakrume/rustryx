import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/privileges')({
  staticData: { crumb: 'nav.items.privileges', access: { area: 'privileges' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.privileges" route="/admin/privileges" milestone="M1" />
  ),
})
