import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/browse')({
  staticData: { crumb: 'pages.browse' },
  component: () => (
    <PlaceholderPage titleKey="pages.browse" route="/repositories/:name/browse" milestone="M1" />
  ),
})
