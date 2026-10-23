import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/anonymous')({
  staticData: { crumb: 'nav.items.anonymousAccess', access: 'admin' },
  component: () => (
    <PlaceholderPage titleKey="nav.items.anonymousAccess" route="/admin/anonymous" milestone="M1" />
  ),
})
