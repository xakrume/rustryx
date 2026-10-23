import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/tasks/$id/runs/$run')({
  staticData: { crumb: 'pages.taskRun' },
  component: () => (
    <PlaceholderPage titleKey="pages.taskRun" route="/admin/tasks/:id/runs/:run" milestone="M1" />
  ),
})
