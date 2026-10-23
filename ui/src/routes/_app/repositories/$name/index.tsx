import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/')({
  component: () => (
    <PlaceholderPage
      titleKey="pages.repositoryOverview"
      route="/repositories/:name"
      milestone="M1"
    />
  ),
})
