import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/tasks/')({
  component: () => (
    <PlaceholderPage titleKey="nav.items.tasks" route="/admin/tasks" milestone="M1" />
  ),
})
