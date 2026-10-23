import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/users/')({
  component: () => (
    <PlaceholderPage titleKey="nav.items.users" route="/admin/users" milestone="M1" />
  ),
})
