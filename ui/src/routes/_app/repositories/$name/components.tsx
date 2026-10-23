import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/components')({
  staticData: { crumb: 'pages.components' },
  component: () => (
    <PlaceholderPage
      titleKey="pages.components"
      route="/repositories/:name/components"
      milestone="M1"
    />
  ),
})
